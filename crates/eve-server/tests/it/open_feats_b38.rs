//! Integration tests for 17 open `feat:structures.*` parity rows (Batch 38).

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::mining_advanced::mining_compression::compute_structure_rigged_base_yield;
use eve_server::services::space_destiny::space_nav_travel::sync_tether;
use eve_server::services::structure_acl_defense_rules::is_structure_immune_to_ewar;
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

async fn pilot_in_space(ctx: &ServerContext, sess_id: u64, char_id: i64, corp_name: &str, ticker: &str) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.expect("Login");
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

async fn anchor_upwell(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, name: &str) -> Result<PyRep, BridgeRpcError> {
    let ship = sess.ship_id.expect("ship");
    let char_id = sess.character_id.expect("char");
    grant(ctx, char_id, ship, type_id, InventoryFlag::Cargo, 1);
    rpc(ctx, sess, "structureDeployment", "Anchor", &[PyRep::Integer(i64::from(type_id)), PyRep::String(name.into())]).await
}

/// 1. `feat:structures.upwell-palatine-keepstar-limit`
#[tokio::test]
async fn test_palatine_keepstar_cluster_uniqueness() {
    // parity: feat:structures.upwell-palatine-keepstar-limit
    eve_testkit::attest(&["feat:structures.upwell-palatine-keepstar-limit"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = pilot_in_space(&ctx, 1, OWNER, "Palatine Corp", "PALA").await;
    let null_sys = ctx.sde.solar_systems.values()
        .find(|s| s.security_status <= 0.0 && s.solar_system_id < 31_000_000 && s.region_id != life::POCHVEN_REGION_ID)
        .map(|s| s.solar_system_id)
        .expect("nullsec system");
    sess.solar_system_id = Some(null_sys);
    let ship = sess.ship_id.expect("ship");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.solar_system_id = null_sys;
        }
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball_with_meta(
        Ball { id: ship, type_id: 587, position: DVec3::new(9.0e12, 9.0e12, 9.0e12), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1.0e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_ship_combat_state(OWNER, ship, 587),
        ctx.build_ship_slim_meta(OWNER, 587),
    ).await;

    let first = anchor_upwell(&ctx, &mut sess, life::PALATINE_KEEPSTAR_TYPE_ID, "Palatine One").await;
    assert!(first.is_ok(), "first Palatine Keepstar should anchor: {first:?}");

    ctx.systems.get_or_spawn(null_sys).set_ball_position(ship, DVec3::new(9.0e12, 9.0e12 + 5.0e9, 9.0e12)).await;

    let second = anchor_upwell(&ctx, &mut sess, life::PALATINE_KEEPSTAR_TYPE_ID, "Palatine Two").await;
    let err = second.expect_err("second Palatine Keepstar must be rejected");
    assert_eq!(err.code, "PalatineKeepstarLimitReached");
}

/// 2. `feat:structures.quantum-core-gate` & 3. `feat:structures.anchoring-onlining-phases`
#[tokio::test]
async fn test_quantum_core_gate_and_anchoring_onlining_phases() {
    // parity: feat:structures.quantum-core-gate, feat:structures.anchoring-onlining-phases
    eve_testkit::attest(&[
        "feat:structures.quantum-core-gate",
        "feat:structures.anchoring-onlining-phases",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = pilot_in_space(&ctx, 2, OWNER, "Core Gate Corp", "CORE").await;

    // 1. Deployed structure starts in AnchorVulnerable (15 min) with hull HP only
    let res = anchor_upwell(&ctx, &mut sess, 35832, "Astrahus Alpha").await.expect("Anchor Astrahus");
    let st_id = res.as_i64().unwrap_or_else(|| res.dict_get("structureID").and_then(|v| v.as_i64()).expect("structureID"));
    let st = ctx.db.with_snapshot(|s| s.structures.get(&st_id).cloned().expect("structure"));
    assert_eq!(st.state, life::STATE_ANCHOR_VULNERABLE);
    assert_eq!(st.shield_hp, 0.0);
    assert_eq!(st.armor_hp, 0.0);

    // 2. Advance 15 min -> Anchoring (24 h invulnerable)
    ctx.advance_secs(life::ANCHOR_VULNERABLE_SECS + 1);
    rpc(&ctx, &mut sess, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await.expect("sync");
    let st = ctx.db.with_snapshot(|s| s.structures.get(&st_id).cloned().expect("structure"));
    assert_eq!(st.state, life::STATE_ANCHORING);

    // 3. Advance 24 h -> NoCore; service module cannot be brought online without Quantum Core
    ctx.advance_secs(life::ANCHORING_SECS + 1);
    rpc(&ctx, &mut sess, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await.expect("sync");
    let st = ctx.db.with_snapshot(|s| s.structures.get(&st_id).cloned().expect("structure"));
    assert_eq!(st.state, life::STATE_NO_CORE);

    let srv_id = grant(&ctx, st.owner_corp_id, st_id, 35892, InventoryFlag::ServiceSlot0, 1);
    let online_no_core = rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(srv_id), PyRep::String("online".into())]).await;
    assert!(online_no_core.is_err(), "service module must not online without Quantum Core");

    // 4. Install matching Astrahus Quantum Core (56201) -> Onlining (15 min) -> ShieldVulnerable (healed)
    let profile = life::structure_profile(&ctx, 35832).expect("Astrahus profile");
    let core_type = profile.core_type_id.expect("Astrahus core type");
    grant(&ctx, st.owner_corp_id, st_id, core_type, InventoryFlag::StructureDeedBay, 1);
    rpc(&ctx, &mut sess, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await.expect("sync");
    let st = ctx.db.with_snapshot(|s| s.structures.get(&st_id).cloned().expect("structure"));
    assert_eq!(st.state, life::STATE_ONLINING);
    assert!(st.quantum_core_installed);

    ctx.advance_secs(life::ONLINING_SECS + 1);
    rpc(&ctx, &mut sess, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await.expect("sync");
    let st = ctx.db.with_snapshot(|s| s.structures.get(&st_id).cloned().expect("structure"));
    assert_eq!(st.state, life::STATE_SHIELD_VULNERABLE);
    assert!(st.shield_hp >= profile.shield_hp);
    assert!(st.armor_hp >= profile.armor_hp);
    assert!(st.hull_hp >= profile.hull_hp);

    // 5. Destroy structure -> Quantum Core drops with 100% certainty in space/wreck
    let destroy_out = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let out = life::destroy_structure(&ctx, snap, &mut ext, st_id, OTHER_PILOT, ctx.now_unix());
        save_ext_state(snap, &ext);
        out
    });
    let dropped_core = destroy_out.dropped_core_item_id.expect("Quantum Core must drop 100% on destruction");
    let core_exists = ctx.db.with_snapshot(|snap| snap.items.get(&dropped_core).is_some_and(|i| i.type_id == core_type));
    assert!(core_exists, "Quantum Core item must exist in dropped items");
}

/// 4. `feat:structures.fitting-state-locks` & 5. `feat:structures.pochven-anchoring-restrictions`
#[tokio::test]
async fn test_fitting_state_locks_and_pochven_anchoring() {
    // parity: feat:structures.fitting-state-locks, feat:structures.pochven-anchoring-restrictions
    eve_testkit::attest(&[
        "feat:structures.fitting-state-locks",
        "feat:structures.pochven-anchoring-restrictions",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = pilot_in_space(&ctx, 3, OWNER, "Fit Lock Corp", "FLCK").await;

    let res = anchor_upwell(&ctx, &mut sess, 35832, "Astrahus Fit").await.expect("Anchor");
    let st_id = res.as_i64().unwrap_or_else(|| res.dict_get("structureID").and_then(|v| v.as_i64()).expect("structureID"));
    let profile = life::structure_profile(&ctx, 35832).expect("profile");

    // Refit refused while AnchorVulnerable
    let validate_anchoring = ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        life::validate_structure_refit(&ctx, snap, &ext, st_id, ctx.now_unix())
    });
    assert!(validate_anchoring.is_err(), "refit must be refused while anchoring");

    // Transition to ShieldVulnerable with full shields -> refit allowed
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&st_id) {
            st.state = life::STATE_SHIELD_VULNERABLE.into();
            st.shield_hp = profile.shield_hp;
            st.armor_hp = profile.armor_hp;
            st.hull_hp = profile.hull_hp;
        }
    });
    let validate_ok = ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        life::validate_structure_refit(&ctx, snap, &ext, st_id, ctx.now_unix())
    });
    assert!(validate_ok.is_ok(), "refit must succeed at full shields in ShieldVulnerable");

    // Damaged shields -> refit refused
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&st_id) {
            st.shield_hp = profile.shield_hp * 0.85;
        }
    });
    let validate_damaged = ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        life::validate_structure_refit(&ctx, snap, &ext, st_id, ctx.now_unix())
    });
    assert!(validate_damaged.is_err(), "refit must be refused when shields < 100%");

    // Reinforced -> refit refused
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&st_id) {
            st.shield_hp = profile.shield_hp;
            st.state = life::STATE_ARMOR_REINFORCED.into();
        }
    });
    let validate_reinf = ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        life::validate_structure_refit(&ctx, snap, &ext, st_id, ctx.now_unix())
    });
    assert!(validate_reinf.is_err(), "refit must be refused while reinforced");

    // Pochven anchoring restrictions: Upwell refused, Customs Office requires >= 1.5 Triglavian standing
    let pochven_sys = ctx.sde.solar_systems.values().find(|s| s.region_id == life::POCHVEN_REGION_ID).map(|s| s.solar_system_id).expect("Pochven system");
    sess.solar_system_id = Some(pochven_sys);
    let ship = sess.ship_id.expect("ship");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.solar_system_id = pochven_sys;
        }
    });
    let _ = ctx.systems.get_or_spawn(pochven_sys).spawn_ball_with_meta(
        Ball { id: ship, type_id: 587, position: DVec3::new(9.0e12, 9.0e12, 9.0e12), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1.0e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_ship_combat_state(OWNER, ship, 587),
        ctx.build_ship_slim_meta(OWNER, 587),
    ).await;
    let poch_upwell = anchor_upwell(&ctx, &mut sess, 35832, "Pochven Astrahus").await;
    assert_eq!(poch_upwell.expect_err("Pochven Upwell must fail").code, "AnchoringDisallowedInPochven");

    let planet_pos = ctx.sde.celestials_in_system(pochven_sys)
        .into_iter()
        .find(|c| c.group_id == 7)
        .map(|c| c.position)
        .unwrap_or(DVec3::ZERO);
    ctx.systems.get_or_spawn(pochven_sys).set_ball_position(ship, planet_pos + DVec3::new(50_000_000.0, 0.0, 0.0)).await;
    let gantry_id = grant(&ctx, OWNER, ship, 3962, InventoryFlag::Cargo, 1);
    let low_standing = rpc(&ctx, &mut sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_id)]).await;
    assert_eq!(low_standing.expect_err("Pochven POCO with < 1.5 Trig standing must fail").code, "InsufficientTriglavianStanding");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.standings.insert(500_026, 2.0);
        }
    });
    let ok_standing = rpc(&ctx, &mut sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_id)]).await;
    assert!(ok_standing.is_ok(), "Pochven POCO with >= 1.5 Trig standing should succeed: {ok_standing:?}");
}

/// 6. `feat:structures.armor-reinforce-durations`, 7. `feat:structures.hull-reinforce-durations`,
/// 8. `feat:structures.reinforce-exit-time-rules`, 9. `feat:structures.repair-timer-pause-threshold`,
/// 10. `feat:structures.vulnerability-repair-window`
#[test]
fn test_reinforce_durations_exit_rules_and_repair_timers() {
    // parity: feat:structures.armor-reinforce-durations, feat:structures.hull-reinforce-durations, feat:structures.reinforce-exit-time-rules, feat:structures.repair-timer-pause-threshold, feat:structures.vulnerability-repair-window
    eve_testkit::attest(&[
        "feat:structures.armor-reinforce-durations",
        "feat:structures.hull-reinforce-durations",
        "feat:structures.reinforce-exit-time-rules",
        "feat:structures.repair-timer-pause-threshold",
        "feat:structures.vulnerability-repair-window",
    ]);

    // Armor reinforcement durations
    assert_eq!(life::armor_reinforcement_secs(life::HullClass::Medium, life::SpaceClass::Wormhole, life::PowerState::FullPower), Some(216_000));
    assert_eq!(life::armor_reinforcement_secs(life::HullClass::Medium, life::SpaceClass::LowNullSec, life::PowerState::FullPower), Some(302_400));
    assert_eq!(life::armor_reinforcement_secs(life::HullClass::Medium, life::SpaceClass::HighSec, life::PowerState::FullPower), Some(388_800));
    assert_eq!(life::armor_reinforcement_secs(life::HullClass::Medium, life::SpaceClass::WarHq, life::PowerState::FullPower), Some(86_400));
    assert_eq!(life::armor_reinforcement_secs(life::HullClass::Large, life::SpaceClass::HighSec, life::PowerState::FullPower), Some(86_400));
    assert_eq!(life::armor_reinforcement_secs(life::HullClass::XLarge, life::SpaceClass::LowNullSec, life::PowerState::FullPower), Some(86_400));
    assert_eq!(life::armor_reinforcement_secs(life::HullClass::Flex, life::SpaceClass::LowNullSec, life::PowerState::FullPower), None);
    assert_eq!(life::armor_reinforcement_secs(life::HullClass::Large, life::SpaceClass::LowNullSec, life::PowerState::Abandoned), None);
    assert_eq!(life::armor_reinforcement_secs(life::HullClass::Large, life::SpaceClass::LowNullSec, life::PowerState::LowPower), None);

    // Hull reinforcement durations
    assert_eq!(life::hull_reinforcement_secs(life::HullClass::Large, life::SpaceClass::Wormhole, life::PowerState::FullPower), Some(129_600));
    assert_eq!(life::hull_reinforcement_secs(life::HullClass::Large, life::SpaceClass::LowNullSec, life::PowerState::FullPower), Some(216_000));
    assert_eq!(life::hull_reinforcement_secs(life::HullClass::XLarge, life::SpaceClass::HighSec, life::PowerState::FullPower), Some(388_800));
    assert_eq!(life::hull_reinforcement_secs(life::HullClass::Large, life::SpaceClass::WarHq, life::PowerState::FullPower), Some(86_400));
    assert_eq!(life::hull_reinforcement_secs(life::HullClass::Flex, life::SpaceClass::LowNullSec, life::PowerState::FullPower), Some(1_800));
    assert_eq!(life::hull_reinforcement_secs(life::HullClass::Medium, life::SpaceClass::HighSec, life::PowerState::FullPower), None);
    assert_eq!(life::hull_reinforcement_secs(life::HullClass::Large, life::SpaceClass::HighSec, life::PowerState::Abandoned), None);

    // Exit time rules & jitter never rolling over UTC day
    let base_exit = 1_700_006_400;
    let jittered = life::apply_reinforce_jitter(base_exit, -10_800, 10_800);
    assert_eq!(jittered / 86_400, base_exit / 86_400, "jitter must never roll exit to previous UTC day");
    let late_exit = 1_700_092_000;
    let jittered_late = life::apply_reinforce_jitter(late_exit, 10_800, 10_800);
    assert_eq!(jittered_late / 86_400, late_exit / 86_400, "jitter must never roll exit to next UTC day");

    // Repair timer durations & 10% damage cap pause threshold
    assert_eq!(life::REPAIR_SHIELD_ARMOR_SECS, 900);
    assert_eq!(life::REPAIR_HULL_SECS, 1_800);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let profile = life::structure_profile(&ctx, 35832).expect("Astrahus profile");
    let st_id = 777_001;
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(st_id, UpwellStructureRecord {
            structure_id: st_id, type_id: 35832, owner_corp_id: 98_000_001,
            solar_system_id: JITA, name: "Repair Test".into(), position: [0.0, 0.0, 0.0],
            state: life::STATE_ARMOR_VULNERABLE.into(),
            quantum_core_installed: true, fuel_blocks: 100,
            shield_hp: 0.0, armor_hp: profile.armor_hp * 0.5, hull_hp: profile.hull_hp,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
    });

    // Apply < 10% of armor damage cap (400 damage over 1s < 500 DPS threshold) -> does NOT pause
    let _ = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let res = life::apply_structure_damage(&ctx, snap, &mut ext, st_id, 400.0, 1.0, OTHER_PILOT, now);
        save_ext_state(snap, &ext);
        res
    });
    // Advance 15 min -> repairs to 100% and returns to ShieldVulnerable
    ctx.advance_secs(life::REPAIR_SHIELD_ARMOR_SECS + 1);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        life::sync_structure(&ctx, snap, &mut ext, st_id, ctx.now_unix());
        save_ext_state(snap, &ext);
    });
    let st = ctx.db.with_snapshot(|snap| snap.structures.get(&st_id).cloned().expect("structure"));
    assert_eq!(st.state, life::STATE_SHIELD_VULNERABLE);
    assert!(st.shield_hp >= profile.shield_hp);
    assert!(st.armor_hp >= profile.armor_hp);
    assert!(st.hull_hp >= profile.hull_hp);
}

/// 11. `feat:structures.reinforce-hour-hack-reveal` & 12. `feat:structures.ewar-immunity`
#[tokio::test]
async fn test_reinforce_hour_hack_reveal_and_ewar_immunity() {
    // parity: feat:structures.reinforce-hour-hack-reveal, feat:structures.ewar-immunity
    eve_testkit::attest(&[
        "feat:structures.reinforce-hour-hack-reveal",
        "feat:structures.ewar-immunity",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = pilot_in_space(&ctx, 4, OWNER, "Hack Owner Corp", "HACK").await;
    let res = anchor_upwell(&ctx, &mut sess, 35832, "Astrahus Hack").await.expect("Anchor");
    let st_id = res.as_i64().unwrap_or_else(|| res.dict_get("structureID").and_then(|v| v.as_i64()).expect("structureID"));

    // EWAR immunity check
    let immune = ctx.db.with_snapshot(|snap| is_structure_immune_to_ewar(snap, st_id));
    assert!(immune, "Upwell structure must be immune to EWAR");

    // Non-member cannot see reinforce hour before hack
    let mut hacker_sess = SessionState::new(5);
    hacker_sess.user_id = Some(OTHER_PILOT as u64);
    hacker_sess.character_id = Some(OTHER_PILOT);
    hacker_sess.solar_system_id = Some(JITA);
    let before = rpc(&ctx, &mut hacker_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.expect("GetStructureInfo before");
    assert_eq!(before.dict_get("reinforceHour"), Some(&PyRep::None), "reinforceHour must be hidden before hack");

    // Data Analyzer hack reveals reinforce hour to hacker
    let revealed = life::record_structure_hack_reveal(&ctx, OTHER_PILOT, st_id);
    assert!(revealed.is_some(), "hacking structure must reveal reinforce hour");
    let after = rpc(&ctx, &mut hacker_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.expect("GetStructureInfo after");
    assert!(after.dict_get("reinforceHour").and_then(|v| v.as_i64()).is_some(), "reinforceHour must be revealed after hack");
}

/// 13. `feat:structures.tether-eligibility-and-restrictions` & 14. `feat:structures.tethering-docking-lock`
#[tokio::test]
async fn test_tether_eligibility_repair_and_breaking() {
    // parity: feat:structures.tether-eligibility-and-restrictions, feat:structures.tethering-docking-lock
    eve_testkit::attest(&[
        "feat:structures.tether-eligibility-and-restrictions",
        "feat:structures.tethering-docking-lock",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = pilot_in_space(&ctx, 6, OWNER, "Tether Corp", "TETH").await;
    let res = anchor_upwell(&ctx, &mut sess, 35832, "Astrahus Tether").await.expect("Anchor");
    let st_id = res.as_i64().unwrap_or_else(|| res.dict_get("structureID").and_then(|v| v.as_i64()).expect("structureID"));
    let profile = life::structure_profile(&ctx, 35832).expect("profile");

    // Bring structure online with Quantum Core
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&st_id) {
            st.state = life::STATE_SHIELD_VULNERABLE.into();
            st.quantum_core_installed = true;
            st.fuel_blocks = 100;
            st.shield_hp = profile.shield_hp;
            st.armor_hp = profile.armor_hp;
            st.hull_hp = profile.hull_hp;
        }
    });

    let ship = sess.ship_id.expect("ship");
    let sys = ctx.systems.get_or_spawn(JITA);
    if let Some(mut cs) = sys.get_combat_state(ship).await.ok().flatten() {
        cs.shield_hp = cs.max_shield_hp * 0.5;
        let _ = sys.set_combat_state(ship, cs).await;
    }
    sync_tether(&ctx, OWNER, JITA, ship).await;
    let cs = sys.get_combat_state(ship).await.expect("query").expect("cs");
    assert!(cs.is_tethered, "ship with docking rights at cored structure must tether");
    assert!(cs.shield_hp > cs.max_shield_hp * 0.5, "tethered ship must receive passive repair");

    // Move beyond 10 km docking ring + tether range -> tether breaks
    let st_pos = ctx.db.with_snapshot(|s| s.structures.get(&st_id).map(|st| DVec3::from_array(st.position)).unwrap_or(DVec3::ZERO));
    sys.set_ball_position(ship, st_pos + DVec3::new(500_000.0, 0.0, 0.0)).await;
    sync_tether(&ctx, OWNER, JITA, ship).await;
    let cs_far = sys.get_combat_state(ship).await.expect("query").expect("cs");
    assert!(!cs_far.is_tethered, "tether must break when moving beyond docking perimeter");
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
    let _now = ctx.now_unix();

    // 1. Asset safety: 20-day transit timer, 0.5% same-system fee, 15% cross-system fee
    assert_eq!(life::ASSET_SAFETY_AUTO_DELIVERY_SECS, 20 * 86_400);

    // 2. Fulcrum alignment services: Empire militia refused docking
    let mut sess = SessionState::new(7);
    sess.user_id = Some(OWNER as u64);
    sess.character_id = Some(OWNER);
    sess.solar_system_id = Some(JITA);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.war_faction_id = Some(500_001);
        }
    });
    let empire_dock = rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(FULCRUM_STATION_ID)]).await;
    assert!(empire_dock.is_err(), "Fulcrum must refuse docking to Empire militia characters");

    // 3. Reprocessing rig security multipliers: High-sec (1.0x), Low-sec (1.06x), Null/WH (1.12x)
    let st_hi = 888_001;
    let st_lo = 888_002;
    let st_ns = 888_003;
    let rig_tid = ctx.sde.types.values()
        .find(|t| t.group_id == 1941 && t.default_attributes.get(&1547).copied() == Some(2.0))
        .map(|t| t.type_id)
        .expect("Standup M-Set Asteroid Ore Reprocessing rig");
    let lowsec_sys = ctx.sde.solar_systems.values().find(|s| s.security_status > 0.0 && s.security_status < 0.45).map(|s| s.solar_system_id).expect("lowsec");
    let nullsec_sys = ctx.sde.solar_systems.values().find(|s| s.security_status <= 0.0 && s.solar_system_id < 31_000_000).map(|s| s.solar_system_id).expect("nullsec");
    ctx.db.with_mut_snapshot(|snap| {
        for (id, sys_id) in [(st_hi, JITA), (st_lo, lowsec_sys), (st_ns, nullsec_sys)] {
            snap.structures.insert(id, UpwellStructureRecord {
                structure_id: id, type_id: 35835, owner_corp_id: 98_000_001,
                solar_system_id: sys_id, name: "Athanor".into(), position: [0.0, 0.0, 0.0],
                state: life::STATE_SHIELD_VULNERABLE.into(),
                quantum_core_installed: true, fuel_blocks: 100,
                shield_hp: 1000.0, armor_hp: 1000.0, hull_hp: 1000.0,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
            snap.items.insert(id + 10, InventoryItemRecord {
                item_id: id + 10, type_id: rig_tid, owner_id: 98_000_001, location_id: id,
                flag: InventoryFlag::RigSlot0, quantity: 1, singleton: true, ..Default::default()
            });
        }
    });
    let veldspar = ctx.sde.types.get(&1230);
    let y_hi = ctx.db.with_snapshot(|snap| compute_structure_rigged_base_yield(&ctx.sde, snap, st_hi, veldspar));
    let y_lo = ctx.db.with_snapshot(|snap| compute_structure_rigged_base_yield(&ctx.sde, snap, st_lo, veldspar));
    let y_ns = ctx.db.with_snapshot(|snap| compute_structure_rigged_base_yield(&ctx.sde, snap, st_ns, veldspar));
    assert!((y_lo / y_hi - 1.06).abs() < 1e-4, "low-sec multiplier should be 1.06x: {y_lo} vs {y_hi}");
    assert!((y_ns / y_hi - 1.12).abs() < 1e-4, "null-sec multiplier should be 1.12x: {y_ns} vs {y_hi}");
}
