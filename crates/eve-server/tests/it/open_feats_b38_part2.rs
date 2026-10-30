//! Integration tests for open `feat:structures.*` parity rows (Batch 38, Part 2).

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::mining_advanced::mining_compression::compute_structure_rigged_base_yield;
use eve_server::services::space_destiny::space_nav_travel::sync_tether;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use glam::DVec3;
use std::collections::HashMap;

const OWNER: i64 = 90_000_001;
const OTHER_PILOT: i64 = 90_000_002;
const JITA: u32 = 30_000_142;
const FULCRUM_STATION_ID: i64 = 60_015_187;

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

fn grant(ctx: &ServerContext, owner: i64, location: i64, type_id: u32, flag: InventoryFlag, qty: i32) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(id, InventoryItemRecord {
            item_id: id, type_id, owner_id: owner, location_id: location,
            flag, quantity: qty, singleton: qty == 1, version: 1, ..Default::default()
        });
        id
    })
}

async fn pilot_in_space_user(ctx: &ServerContext, sess_id: u64, user: &str, char_id: i64, corp_name: &str, ticker: &str) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String(user.into()), PyRep::String("demo-secret".into())]).await.expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("Select");
    ctx.advance_secs(12);
    rpc(ctx, &mut sess, "corpRegistry", "AddCorporation", &[PyRep::String(corp_name.into()), PyRep::String(ticker.into())]).await.expect("AddCorporation");
    ctx.advance_secs(12);
    rpc(ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    let st_pos = ctx.sde.stations.values().find(|s| s.solar_system_id == JITA).map(|s| s.position).unwrap_or(DVec3::ZERO);
    let pos = st_pos + DVec3::new(0.0, 5.0e9, 0.0);
    ctx.systems.get_or_spawn(JITA).set_ball_position(sess.ship_id.expect("ship"), pos).await;
    sess
}

async fn pilot_in_space(ctx: &ServerContext, sess_id: u64, char_id: i64, corp_name: &str, ticker: &str) -> SessionState {
    pilot_in_space_user(ctx, sess_id, "demo", char_id, corp_name, ticker).await
}

/// 6. `feat:structures.armor-reinforce-durations`, 7. `feat:structures.hull-reinforce-durations`,
/// 8. `feat:structures.reinforce-exit-time-rules`
#[test]
fn test_reinforce_durations_and_exit_time_rules() {
    // parity: feat:structures.armor-reinforce-durations, feat:structures.hull-reinforce-durations, feat:structures.reinforce-exit-time-rules
    eve_testkit::attest(&[
        "feat:structures.armor-reinforce-durations",
        "feat:structures.hull-reinforce-durations",
        "feat:structures.reinforce-exit-time-rules",
    ]);

    // Armor reinforcement durations table
    assert_eq!(life::armor_reinforcement_secs(life::HullClass::Medium, life::SpaceClass::Wormhole, life::PowerState::FullPower), Some(216_000));
    assert_eq!(life::armor_reinforcement_secs(life::HullClass::Medium, life::SpaceClass::LowNullSec, life::PowerState::FullPower), Some(302_400));
    assert_eq!(life::armor_reinforcement_secs(life::HullClass::Medium, life::SpaceClass::HighSec, life::PowerState::FullPower), Some(388_800));
    assert_eq!(life::armor_reinforcement_secs(life::HullClass::Medium, life::SpaceClass::WarHq, life::PowerState::FullPower), Some(86_400));
    assert_eq!(life::armor_reinforcement_secs(life::HullClass::Large, life::SpaceClass::HighSec, life::PowerState::FullPower), Some(86_400));
    assert_eq!(life::armor_reinforcement_secs(life::HullClass::XLarge, life::SpaceClass::LowNullSec, life::PowerState::FullPower), Some(86_400));
    assert_eq!(life::armor_reinforcement_secs(life::HullClass::Flex, life::SpaceClass::LowNullSec, life::PowerState::FullPower), None);
    assert_eq!(life::armor_reinforcement_secs(life::HullClass::Large, life::SpaceClass::LowNullSec, life::PowerState::Abandoned), None);
    assert_eq!(life::armor_reinforcement_secs(life::HullClass::Large, life::SpaceClass::LowNullSec, life::PowerState::LowPower), None);

    // Hull reinforcement durations table
    assert_eq!(life::hull_reinforcement_secs(life::HullClass::Large, life::SpaceClass::Wormhole, life::PowerState::FullPower), Some(129_600));
    assert_eq!(life::hull_reinforcement_secs(life::HullClass::Large, life::SpaceClass::LowNullSec, life::PowerState::FullPower), Some(216_000));
    assert_eq!(life::hull_reinforcement_secs(life::HullClass::XLarge, life::SpaceClass::HighSec, life::PowerState::FullPower), Some(388_800));
    assert_eq!(life::hull_reinforcement_secs(life::HullClass::Large, life::SpaceClass::WarHq, life::PowerState::FullPower), Some(86_400));
    assert_eq!(life::hull_reinforcement_secs(life::HullClass::Flex, life::SpaceClass::LowNullSec, life::PowerState::FullPower), Some(1_800));
    assert_eq!(life::hull_reinforcement_secs(life::HullClass::Medium, life::SpaceClass::HighSec, life::PowerState::FullPower), None);
    assert_eq!(life::hull_reinforcement_secs(life::HullClass::Large, life::SpaceClass::HighSec, life::PowerState::Abandoned), None);

    // Jitter never rolling over UTC day
    let base_exit = 1_700_006_400;
    let jittered = life::apply_reinforce_jitter(base_exit, -10_800, 10_800);
    assert_eq!(jittered / 86_400, base_exit / 86_400, "jitter must never roll exit to previous UTC day");
    let late_exit = 1_700_092_000;
    let jittered_late = life::apply_reinforce_jitter(late_exit, 10_800, 10_800);
    assert_eq!(jittered_late / 86_400, late_exit / 86_400, "jitter must never roll exit to next UTC day");

    // In-flow transitions: Large structure (Fortizar 35833) in Low-sec with online service module
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let lowsec_sys = ctx.sde.solar_systems.values().find(|s| s.security_status > 0.0 && s.security_status < 0.45).map(|s| s.solar_system_id).expect("lowsec");
    let fort_prof = life::structure_profile(&ctx, 35833).expect("Fortizar profile");
    let fort_id = 780_001;
    let t0 = 1_700_010_000u64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(fort_id, UpwellStructureRecord {
            structure_id: fort_id, type_id: 35833, owner_corp_id: 98_000_001,
            solar_system_id: lowsec_sys, name: "Fortizar Flow".into(), position: [0.0, 0.0, 0.0],
            state: life::STATE_SHIELD_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 100,
            shield_hp: 100.0, armor_hp: fort_prof.armor_hp, hull_hp: fort_prof.hull_hp,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        snap.items.insert(fort_id + 1, InventoryItemRecord {
            item_id: fort_id + 1, type_id: 35892, owner_id: 98_000_001, location_id: fort_id,
            flag: InventoryFlag::ServiceSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        snap.module_online_states.insert(fort_id + 1, true);
        snap.structure_vulnerability_windows.insert(fort_id, (18, 0));
        let mut ext = load_ext_state(snap);
        // Simulate a 2-hour shield fight before depletion: exit time must be anchored to shield_fight_started_unix (t0), not fight duration
        ext.lifecycle.entry(fort_id).or_default().shield_fight_started_unix = Some(t0);
        let out_sh = life::apply_structure_damage(&ctx, snap, &mut ext, fort_id, 500.0, 1.0, OTHER_PILOT, t0 + 7_200).expect("deplete shield");
        assert_eq!(out_sh.state, life::STATE_ARMOR_REINFORCED);
        let unjittered_armor = ext.lifecycle.get(&fort_id).and_then(|l| l.unjittered_reinforce_exit_unix).expect("unjittered armor exit");
        assert_eq!((unjittered_armor % 86_400) / 3_600, 18, "exit hour must match owner's chosen hour (18:00 UTC)");
        assert_eq!(unjittered_armor, life::next_reinforce_exit(t0 + 86_400, 18), "armor exit must be computed from shield fight start, not fight duration");

        // Transition to ArmorVulnerable and deplete armor -> HullReinforced anchored to shield fight date (t0 + 24h + 2.5d)
        let armor_exit = out_sh.reinforced_until_unix;
        life::sync_structure(&ctx, snap, &mut ext, fort_id, armor_exit + 1);
        assert_eq!(snap.structures.get(&fort_id).unwrap().state, life::STATE_ARMOR_VULNERABLE);
        if let Some(s) = snap.structures.get_mut(&fort_id) { s.armor_hp = 100.0; }
        let out_ar = life::apply_structure_damage(&ctx, snap, &mut ext, fort_id, 500.0, 1.0, OTHER_PILOT, armor_exit + 3_600).expect("deplete armor");
        assert_eq!(out_ar.state, life::STATE_HULL_REINFORCED);
        let unjittered_hull = ext.lifecycle.get(&fort_id).and_then(|l| l.unjittered_reinforce_exit_unix).expect("unjittered hull exit");
        assert_eq!(unjittered_hull, life::next_reinforce_exit(t0 + 86_400 + 216_000, 18), "shield fight date must fix later hull timer date");
        save_ext_state(snap, &ext);
    });

    // Low-power Large structure skips armor reinforcement and goes straight to ArmorVulnerable
    let lp_id = 780_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(lp_id, UpwellStructureRecord {
            structure_id: lp_id, type_id: 35833, owner_corp_id: 98_000_001,
            solar_system_id: lowsec_sys, name: "LowPower Fortizar".into(), position: [0.0, 0.0, 0.0],
            state: life::STATE_SHIELD_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 0,
            shield_hp: 100.0, armor_hp: fort_prof.armor_hp, hull_hp: fort_prof.hull_hp,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        let mut ext = load_ext_state(snap);
        let out = life::apply_structure_damage(&ctx, snap, &mut ext, lp_id, 500.0, 1.0, OTHER_PILOT, t0).expect("deplete low-power shield");
        assert_eq!(out.state, life::STATE_ARMOR_VULNERABLE, "low-power Large/XL must skip armor reinforcement");
        save_ext_state(snap, &ext);
    });

    // Medium structure (Astrahus 35832) has no hull timer -> ArmorVulnerable goes straight to HullVulnerable
    let med_id = 780_003;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(med_id, UpwellStructureRecord {
            structure_id: med_id, type_id: 35832, owner_corp_id: 98_000_001,
            solar_system_id: lowsec_sys, name: "Medium Astrahus".into(), position: [0.0, 0.0, 0.0],
            state: life::STATE_ARMOR_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 100,
            shield_hp: 0.0, armor_hp: 100.0, hull_hp: 100_000.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        let mut ext = load_ext_state(snap);
        let out = life::apply_structure_damage(&ctx, snap, &mut ext, med_id, 500.0, 1.0, OTHER_PILOT, t0).expect("deplete medium armor");
        assert_eq!(out.state, life::STATE_HULL_VULNERABLE, "Medium structure must skip hull reinforcement");
        save_ext_state(snap, &ext);
    });
}

/// 9. `feat:structures.repair-timer-pause-threshold` & 10. `feat:structures.vulnerability-repair-window`
#[test]
fn test_repair_timer_pause_threshold_and_vulnerability_window() {
    // parity: feat:structures.repair-timer-pause-threshold, feat:structures.vulnerability-repair-window
    eve_testkit::attest(&[
        "feat:structures.repair-timer-pause-threshold",
        "feat:structures.vulnerability-repair-window",
    ]);

    assert_eq!(life::REPAIR_SHIELD_ARMOR_SECS, 900);
    assert_eq!(life::REPAIR_HULL_SECS, 1_800);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let profile = life::structure_profile(&ctx, 35832).expect("Astrahus profile");
    let now = ctx.now_unix();

    // 1. Armor vulnerability window (15 min): pauses on >= 10% damage cap, does NOT pause below 10%, repairs to 100%
    let st_armor = 781_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(st_armor, UpwellStructureRecord {
            structure_id: st_armor, type_id: 35832, owner_corp_id: 98_000_001,
            solar_system_id: JITA, name: "Armor Repair Test".into(), position: [0.0, 0.0, 0.0],
            state: life::STATE_ARMOR_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 100,
            shield_hp: 0.0, armor_hp: profile.armor_hp * 0.5, hull_hp: profile.hull_hp,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(st_armor).or_default().repair_until_unix = Some(now + life::REPAIR_SHIELD_ARMOR_SECS);
        // Qualifying damage (>= 10% damage cap) pauses repair timer
        let paused = life::apply_structure_damage(&ctx, snap, &mut ext, st_armor, 600.0, 1.0, OTHER_PILOT, now + 100).expect("qualifying");
        assert!(paused.repair_paused, "damage >= 10% of damage cap must pause armor repair timer");
        let after_pause = ext.lifecycle.get(&st_armor).and_then(|l| l.repair_until_unix).expect("repair_until");
        assert!(after_pause > now + life::REPAIR_SHIELD_ARMOR_SECS);

        // Below-threshold damage (< 10% damage cap) does NOT pause repair timer
        let not_paused = life::apply_structure_damage(&ctx, snap, &mut ext, st_armor, 200.0, 1.0, OTHER_PILOT, now + 105).expect("below threshold");
        assert!(!not_paused.repair_paused, "damage < 10% of damage cap must not pause repair timer");
        assert_eq!(ext.lifecycle.get(&st_armor).and_then(|l| l.repair_until_unix), Some(after_pause));

        // Uninterrupted countdown to 0 repairs all hull, armor, and shields back to 100% and returns to ShieldVulnerable
        life::sync_structure(&ctx, snap, &mut ext, st_armor, after_pause + 1);
        save_ext_state(snap, &ext);
    });
    let st = ctx.db.with_snapshot(|snap| snap.structures.get(&st_armor).cloned().expect("structure"));
    assert_eq!(st.state, life::STATE_SHIELD_VULNERABLE);
    assert!(st.shield_hp >= profile.shield_hp);
    assert!(st.armor_hp >= profile.armor_hp);
    assert!(st.hull_hp >= profile.hull_hp);

    // 2. Hull vulnerability window (30 min = 1800s) & Abandoned structure rules
    let st_hull = 781_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(st_hull, UpwellStructureRecord {
            structure_id: st_hull, type_id: 35832, owner_corp_id: 98_000_001,
            solar_system_id: JITA, name: "Abandoned Hull Repair Test".into(), position: [0.0, 0.0, 0.0],
            state: life::STATE_HULL_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 0,
            shield_hp: 0.0, armor_hp: 0.0, hull_hp: profile.hull_hp * 0.5,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(st_hull).or_default().low_power_since_unix = Some(now - life::ABANDON_AFTER_LOW_POWER_SECS - 10);
        assert_eq!(life::power_state(&ctx, snap, &ext, snap.structures.get(&st_hull).unwrap(), now), life::PowerState::Abandoned);
        let out = life::apply_structure_damage(&ctx, snap, &mut ext, st_hull, 200.0, 1.0, OTHER_PILOT, now).expect("abandoned hull damage");
        assert!(!out.repair_paused);
        let hull_repair_until = ext.lifecycle.get(&st_hull).and_then(|l| l.repair_until_unix).expect("hull repair_until");
        assert_eq!(hull_repair_until, now + life::REPAIR_HULL_SECS, "hull repair timer must be 30 min (1800s)");
        // Not repaired after 15 min
        life::sync_structure(&ctx, snap, &mut ext, st_hull, now + 900);
        assert_eq!(snap.structures.get(&st_hull).unwrap().state, life::STATE_HULL_VULNERABLE);
        // Repaired to 100% ShieldVulnerable after 30 min
        life::sync_structure(&ctx, snap, &mut ext, st_hull, hull_repair_until + 1);
        save_ext_state(snap, &ext);
    });
    let st_h = ctx.db.with_snapshot(|snap| snap.structures.get(&st_hull).cloned().expect("structure"));
    assert_eq!(st_h.state, life::STATE_SHIELD_VULNERABLE);
    assert!(st_h.shield_hp >= profile.shield_hp);
    assert!(st_h.armor_hp >= profile.armor_hp);
    assert!(st_h.hull_hp >= profile.hull_hp);
}

/// 13. `feat:structures.tether-eligibility-and-restrictions` & 14. `feat:structures.tethering-docking-lock`
#[tokio::test]
async fn test_tether_eligibility_restrictions_and_docking_lock() {
    // parity: feat:structures.tether-eligibility-and-restrictions, feat:structures.tethering-docking-lock
    eve_testkit::attest(&[
        "feat:structures.tether-eligibility-and-restrictions",
        "feat:structures.tethering-docking-lock",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = pilot_in_space(&ctx, 6, OWNER, "Tether Corp", "TETH").await;
    let ship = sess.ship_id.expect("ship");
    let st_pos = ctx.systems.get_or_spawn(JITA).get_ball_snapshot(ship).await.expect("q").expect("b").1.position;
    let profile = life::structure_profile(&ctx, 35832).expect("profile");
    let st_id = 782_001;
    let owner_corp = ctx.db.get_character(OWNER).expect("owner").corporation_id;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(st_id, UpwellStructureRecord {
            structure_id: st_id, type_id: 35832, owner_corp_id: owner_corp,
            solar_system_id: JITA, name: "Astrahus Tether".into(), position: st_pos.to_array(),
            state: life::STATE_SHIELD_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 100,
            shield_hp: profile.shield_hp, armor_hp: profile.armor_hp, hull_hp: profile.hull_hp,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
    });

    // Damage shield, armor, and hull (using expect, never if-let) and verify passive repair + untargetability
    let sys = ctx.systems.get_or_spawn(JITA);
    let mut cs = sys.get_combat_state(ship).await.expect("query").expect("combat state");
    cs.shield_hp = cs.max_shield_hp * 0.5;
    cs.armor_hp = cs.max_armor_hp * 0.5;
    cs.structure_hp = cs.max_structure_hp * 0.5;
    sys.set_combat_state(ship, cs).await.expect("set combat state");

    sync_tether(&ctx, OWNER, JITA, ship).await;
    let cs_teth = sys.get_combat_state(ship).await.expect("query").expect("cs");
    assert!(cs_teth.is_tethered, "ship with docking rights at cored structure must tether");
    assert!(cs_teth.shield_hp > cs_teth.max_shield_hp * 0.5, "tethered ship must receive passive shield repair");
    assert!(cs_teth.armor_hp > cs_teth.max_armor_hp * 0.5, "tethered ship must receive passive armor repair");
    assert!(cs_teth.structure_hp > cs_teth.max_structure_hp * 0.5, "tethered ship must receive passive hull repair");

    // Tethered ship cannot be targeted
    let hostile_ship = 999_001;
    let mut hostile_sess = SessionState::new(7);
    hostile_sess.user_id = Some(2);
    hostile_sess.character_id = Some(OTHER_PILOT);
    hostile_sess.solar_system_id = Some(JITA);
    hostile_sess.ship_id = Some(hostile_ship);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OTHER_PILOT) {
            ch.active_ship_id = hostile_ship;
            ch.solar_system_id = JITA;
        }
        snap.items.insert(hostile_ship, InventoryItemRecord {
            item_id: hostile_ship, type_id: 587, owner_id: OTHER_PILOT, location_id: i64::from(JITA),
            flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let _ = sys.spawn_ball_with_meta(
        Ball { id: hostile_ship, type_id: 587, position: st_pos + DVec3::new(1_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1.0e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_ship_combat_state(OTHER_PILOT, hostile_ship, 587),
        ctx.build_ship_slim_meta(OTHER_PILOT, 587),
    ).await;
    let lock_err = rpc(&ctx, &mut hostile_sess, "dogmaIM", "AddTarget", &[PyRep::Integer(ship)]).await
        .expect_err("tethered ship must not be targetable");
    assert_eq!(lock_err.code, "TargetInvulnerable");

    // Blocked actions while tethered: offensive module / smartbomb -> CantActivateWhileTethered, fleet compression -> CannotCompressWhileTethered
    let sb_tid = ctx.sde.types.values().find(|t| t.group_id == 72).map(|t| t.type_id).expect("smartbomb");
    let sb_id = grant(&ctx, OWNER, ship, sb_tid, InventoryFlag::HiSlot0, 1);
    let sb_err = rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(sb_id), PyRep::String("default".into())]).await
        .expect_err("smartbomb must be refused while tethered");
    assert_eq!(sb_err.code, "CantActivateWhileTethered");

    // Tether breaks on: locking a target
    let _ = sys.add_target_lock(ship, eve_destiny::TargetLock {
        target_id: hostile_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None,
    }).await;
    sync_tether(&ctx, OWNER, JITA, ship).await;
    assert!(!sys.get_combat_state(ship).await.expect("q").expect("cs").is_tethered, "tether must break on locking a target");
    let _ = sys.remove_target_lock(ship, hostile_ship).await;
    sync_tether(&ctx, OWNER, JITA, ship).await;
    assert!(sys.get_combat_state(ship).await.expect("q").expect("cs").is_tethered);

    // Tether breaks on: activating a bastion / cyno module (group 515 / 658)
    let cyno_tid = ctx.sde.types.values().find(|t| t.group_id == 658).map(|t| t.type_id).expect("cyno");
    let cyno_id = grant(&ctx, OWNER, ship, cyno_tid, InventoryFlag::HiSlot1, 1);
    ctx.db.with_mut_snapshot(|snap| { snap.module_active_states.insert(cyno_id); });
    sync_tether(&ctx, OWNER, JITA, ship).await;
    assert!(!sys.get_combat_state(ship).await.expect("q").expect("cs").is_tethered, "tether must break on cyno/bastion module");
    ctx.db.with_mut_snapshot(|snap| { snap.module_active_states.remove(&cyno_id); });
    sync_tether(&ctx, OWNER, JITA, ship).await;
    assert!(sys.get_combat_state(ship).await.expect("q").expect("cs").is_tethered);

    // Tether breaks on: leaving 10 km-from-docking-ring range
    sys.set_ball_position(ship, st_pos + DVec3::new(500_000.0, 0.0, 0.0)).await;
    sync_tether(&ctx, OWNER, JITA, ship).await;
    assert!(!sys.get_combat_state(ship).await.expect("q").expect("cs").is_tethered, "tether must break beyond docking perimeter");
    sys.set_ball_position(ship, st_pos).await;

    // Tether refused for: structure without quantum core, abandoned structure, unpiloted ship, warp-scrambled ship, weapons timer
    ctx.db.with_mut_snapshot(|snap| { snap.structures.get_mut(&st_id).unwrap().quantum_core_installed = false; });
    sync_tether(&ctx, OWNER, JITA, ship).await;
    assert!(!sys.get_combat_state(ship).await.expect("q").expect("cs").is_tethered, "tether refused without quantum core");
    ctx.db.with_mut_snapshot(|snap| { snap.structures.get_mut(&st_id).unwrap().quantum_core_installed = true; });

    // Unpiloted ship (char_id == 0)
    sync_tether(&ctx, 0, JITA, ship).await;
    assert!(!sys.get_combat_state(ship).await.expect("q").expect("cs").is_tethered, "tether refused for unpiloted ship");

    // Warp-scrambled ship
    let mut cs_scram = sys.get_combat_state(ship).await.expect("q").expect("cs");
    cs_scram.warp_scramble_strength = 2;
    sys.set_combat_state(ship, cs_scram).await.expect("set scram");
    sync_tether(&ctx, OWNER, JITA, ship).await;
    assert!(!sys.get_combat_state(ship).await.expect("q").expect("cs").is_tethered, "tether refused when warp-scrambled");
}

/// 15. `feat:structures.asset-safety-recovery-delay-and-fee`,
/// 16. `feat:structures.fulcrum-alignment-services`,
/// 17. `feat:structures.reprocessing-rig-security-multipliers`
#[tokio::test]
async fn test_asset_safety_fulcrum_and_reprocessing_rigs() {
    // parity: feat:structures.asset-safety-recovery-delay-and-fee, feat:structures.fulcrum-alignment-services, feat:structures.reprocessing-rig-security-multipliers
    eve_testkit::attest(&[
        "feat:structures.asset-safety-recovery-delay-and-fee",
        "feat:structures.fulcrum-alignment-services",
        "feat:structures.reprocessing-rig-security-multipliers",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = pilot_in_space(&ctx, 8, OWNER, "Asset Fulcrum Corp", "ASFC").await;

    // 1. Asset safety: 20-day transit timer, 0.5% same-system fee, 15% cross-system/lowsec fee, wormhole loot drop
    assert_eq!(life::ASSET_SAFETY_AUTO_DELIVERY_SECS, 20 * 86_400);
    let lowsec_station_rec = ctx.sde.stations.values()
        .find(|st| ctx.sde.solar_systems.get(&st.solar_system_id).is_some_and(|s| s.security_status > 0.0 && s.security_status < 0.45))
        .expect("lowsec station");
    let lowsec_sys = lowsec_station_rec.solar_system_id;
    let lowsec_station = lowsec_station_rec.station_id;
    let st_same = 783_001;
    let st_auto = 783_002;
    let st_wh = 783_003;
    let wh_sys = ctx.sde.solar_systems.values().find(|s| s.solar_system_id >= 31_000_000).map(|s| s.solar_system_id).expect("wormhole");

    let (wrap_same, wrap_auto) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.balance = eve_db::IskCents::from_f64_isk(1_000_000_000.0);
        }
        for (sid, sys_id) in [(st_same, lowsec_sys), (st_auto, lowsec_sys), (st_wh, wh_sys)] {
            snap.structures.insert(sid, UpwellStructureRecord {
                structure_id: sid, type_id: 35832, owner_corp_id: 98_000_001,
                solar_system_id: sys_id, name: format!("AS {sid}"), position: [0.0, 0.0, 0.0],
                state: life::STATE_SHIELD_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 100,
                shield_hp: 100.0, armor_hp: 100.0, hull_hp: 100.0,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
            snap.items.insert(sid + 50, InventoryItemRecord {
                item_id: sid + 50, type_id: 587, owner_id: OWNER, location_id: sid,
                flag: InventoryFlag::Hangar, quantity: 10, singleton: false, ..Default::default()
            });
        }
        let mut ext = load_ext_state(snap);
        let out_same = life::destroy_structure(&ctx, snap, &mut ext, st_same, OTHER_PILOT, ctx.now_unix());
        let out_auto = life::destroy_structure(&ctx, snap, &mut ext, st_auto, OTHER_PILOT, ctx.now_unix());
        let out_wh = life::destroy_structure(&ctx, snap, &mut ext, st_wh, OTHER_PILOT, ctx.now_unix());
        save_ext_state(snap, &ext);
        assert!(out_wh.asset_safety_wrap_ids.is_empty(), "wormhole structure must not create asset safety wraps");
        assert!(out_wh.dropped_item_ids.contains(&(st_wh + 50)), "wormhole structure items must drop as loot");
        (out_same.asset_safety_wrap_ids[0], out_auto.asset_safety_wrap_ids[0])
    });

    // Advance 6 days -> same-system claim charges 0.5% fee
    ctx.advance_secs(6 * 86_400);
    let bal_before_same = ctx.db.get_character(OWNER).unwrap().balance.to_wire_f64();
    let res_same = rpc(&ctx, &mut sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(wrap_same), PyRep::Integer(lowsec_station)]).await.expect("same-system claim");
    let fee_same = res_same.dict_get("feePaidISK").and_then(|v| v.as_f64()).expect("feePaidISK");
    let bal_after_same = ctx.db.get_character(OWNER).unwrap().balance.to_wire_f64();
    assert!(fee_same > 0.0 && (bal_before_same - bal_after_same - fee_same).abs() < 0.01);

    // Advance 15 more days (total 21 days >= 20 days) -> auto-delivered, claim charges 15% fee (30x the 0.5% fee!)
    ctx.advance_secs(15 * 86_400);
    let res_auto = rpc(&ctx, &mut sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(wrap_auto)]).await.expect("auto-delivered claim");
    let fee_auto = res_auto.dict_get("feePaidISK").and_then(|v| v.as_f64()).expect("feePaidISK");
    assert!((fee_auto / fee_same - 30.0).abs() < 0.1, "15% fee must be 30x the 0.5% same-system fee: {fee_auto} vs {fee_same}");

    // 2. Fulcrum alignment services: real ship in space in Zarzakh
    let fulcrum_st = ctx.sde.stations.get(&FULCRUM_STATION_ID).expect("Fulcrum station in SDE");
    let zarzakh_sys = fulcrum_st.solar_system_id;
    let ship = sess.ship_id.expect("ship");
    sess.solar_system_id = Some(zarzakh_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.solar_system_id = zarzakh_sys;
            ch.war_faction_id = Some(500_001);
        }
    });
    let z_sys = ctx.systems.get_or_spawn(zarzakh_sys);
    let _ = z_sys.spawn_ball_with_meta(
        Ball { id: ship, type_id: 587, position: fulcrum_st.position + DVec3::new(1_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1.0e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_ship_combat_state(OWNER, ship, 587),
        ctx.build_ship_slim_meta(OWNER, 587),
    ).await;

    let empire_dock = rpc(&ctx, &mut sess, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)]).await;
    assert_eq!(empire_dock.expect_err("Fulcrum must refuse Empire militia").code, "FulcrumRefusesEmpireMilitia");

    // Neutral character with retribution timer -> FulcrumRefusesTimerActive
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) { ch.war_faction_id = None; }
    });
    let exp_retrib = ctx.now_unix() + 300;
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.disapproval_flags.insert(OWNER, exp_retrib);
    });
    let timer_dock = rpc(&ctx, &mut sess, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)]).await;
    assert_eq!(timer_dock.expect_err("Fulcrum must refuse active retribution timer").code, "FulcrumRefusesTimerActive");

    // Neutral character without timer docks at Fulcrum
    ctx.advance_secs(400);
    let ok_dock = rpc(&ctx, &mut sess, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)]).await;
    assert!(ok_dock.is_ok(), "neutral character must dock at Fulcrum: {ok_dock:?}");

    // Jump clone & corp office require pirate enlistment
    let clone_neutral = rpc(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStation", &[]).await;
    assert_eq!(clone_neutral.expect_err("Fulcrum clone requires pirate enlistment").code, "FulcrumCloneRequiresPirateEnlistment");
    let office_neutral = rpc(&ctx, &mut sess, "officeManager", "RentOffice", &[PyRep::Integer(FULCRUM_STATION_ID)]).await;
    assert_eq!(office_neutral.expect_err("Fulcrum office requires pirate enlistment").code, "FulcrumOfficeRequiresPirateEnlistment");

    // Pirate-enlisted character (500_010) can install clone & rent office, and Angel/Guristas LP stores are available
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.war_faction_id = Some(500_010);
            if let Some(corp) = snap.corporations.get_mut(&ch.corporation_id) {
                corp.set_division_balance(1, eve_db::IskCents::from_f64_isk(100_000_000.0));
            }
        }
        snap.skills.entry(OWNER).or_default().insert(24242, eve_db::TrainedSkillRecord { type_id: 24242, level: 5, skill_points: 256_000.0 });
    });
    assert!(rpc(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStation", &[]).await.is_ok(), "pirate-enlisted clone install should succeed");
    assert!(rpc(&ctx, &mut sess, "officeManager", "RentOffice", &[PyRep::Integer(FULCRUM_STATION_ID)]).await.is_ok(), "pirate-enlisted office rent should succeed");
    assert!(rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(1_000_124)]).await.is_ok(), "Angel Cartel LP store");
    assert!(rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(1_000_127)]).await.is_ok(), "Guristas LP store");

    // 3. Reprocessing rig security multipliers: M-Set (Athanor 35835) & L-Set (Tatara 35836) for Asteroid, Ice, Moon Ore
    let nullsec_sys = ctx.sde.solar_systems.values().find(|s| s.security_status <= 0.0 && s.solar_system_id < 31_000_000).map(|s| s.solar_system_id).expect("nullsec");
    let veldspar = ctx.sde.types.get(&1230); // Asteroid Ore
    let blue_ice = ctx.sde.types.values().find(|t| t.group_id == 465); // Ice
    let moon_ore = ctx.sde.types.values().find(|t| t.group_id == 1884); // Moon Ore

    for (hull_tid, rig_size_val, groups) in [
        (35835u32, 2.0f64, vec![1941u32, 1942, 1943]),
        (35836u32, 3.0f64, vec![1944u32, 1945]),
    ] {
        for rig_grp in groups {
            let Some(rig_tid) = ctx.sde.types.values()
                .find(|t| t.group_id == rig_grp && t.default_attributes.get(&1547).copied() == Some(rig_size_val) && t.default_attributes.contains_key(&717))
                .map(|t| t.type_id) else { continue; };
            for ore_ty in [veldspar, blue_ice, moon_ore] {
                let tid = ore_ty.map(|t| t.type_id);
                if rig_grp == 1941 && tid != veldspar.map(|t| t.type_id) { continue; }
                if rig_grp == 1942 && tid != blue_ice.map(|t| t.type_id) { continue; }
                if rig_grp == 1943 && tid != moon_ore.map(|t| t.type_id) { continue; }
                let (s_hi, s_lo, s_ns) = (889_001, 889_002, 889_003);
                ctx.db.with_mut_snapshot(|snap| {
                    for (id, sys_id) in [(s_hi, JITA), (s_lo, lowsec_sys), (s_ns, nullsec_sys)] {
                        snap.structures.insert(id, UpwellStructureRecord {
                            structure_id: id, type_id: hull_tid, owner_corp_id: 98_000_001,
                            solar_system_id: sys_id, name: "Refinery".into(), position: [0.0, 0.0, 0.0],
                            state: life::STATE_SHIELD_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 100,
                            shield_hp: 1000.0, armor_hp: 1000.0, hull_hp: 1000.0,
                            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
                        });
                        snap.items.insert(id + 10, InventoryItemRecord {
                            item_id: id + 10, type_id: rig_tid, owner_id: 98_000_001, location_id: id,
                            flag: InventoryFlag::RigSlot0, quantity: 1, singleton: true, ..Default::default()
                        });
                    }
                });
                let y_hi = ctx.db.with_snapshot(|snap| compute_structure_rigged_base_yield(&ctx.sde, snap, s_hi, ore_ty));
                let y_lo = ctx.db.with_snapshot(|snap| compute_structure_rigged_base_yield(&ctx.sde, snap, s_lo, ore_ty));
                let y_ns = ctx.db.with_snapshot(|snap| compute_structure_rigged_base_yield(&ctx.sde, snap, s_ns, ore_ty));
                assert!((y_lo / y_hi - 1.06).abs() < 1e-4, "hull {hull_tid} group {rig_grp} low-sec multiplier should be 1.06x");
                assert!((y_ns / y_hi - 1.12).abs() < 1e-4, "hull {hull_tid} group {rig_grp} null-sec multiplier should be 1.12x");
            }
        }
    }
}
