//! Integration tests for open `feat:structures.*` parity rows (Batch 38, Part 1).

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use glam::DVec3;
use std::collections::HashMap;

const OWNER: i64 = 90_000_001;
const OTHER_PILOT: i64 = 90_000_002;
const JITA: u32 = 30_000_142;

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

fn extract_id(rep: &PyRep) -> i64 {
    rep.as_i64().unwrap_or_else(|| rep.dict_get("structureID").and_then(|v| v.as_i64()).expect("structureID"))
}

#[tokio::test]
async fn test_palatine_keepstar_cluster_uniqueness() {

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

#[tokio::test]
async fn test_quantum_core_gate_and_anchoring_onlining_phases() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = pilot_in_space(&ctx, 2, OWNER, "Core Gate Corp", "CORE").await;

    // Verify all 8 Upwell hulls require distinct matching Quantum Cores
    let hulls = [35832u32, 35833, 35834, 35825, 35826, 35827, 35835, 35836];
    let mut core_ids = std::collections::HashSet::new();
    for h in hulls {
        let cid = life::structure_profile(&ctx, h).expect("hull profile").core_type_id.expect("core_type_id");
        assert!(core_ids.insert(cid), "hull {h} must have a unique Quantum Core type");
    }

    // 1. Deployed structure starts in AnchorVulnerable (15 min) with hull HP only
    let st_id = extract_id(&anchor_upwell(&ctx, &mut sess, 35832, "Astrahus Alpha").await.expect("Anchor Astrahus"));
    let profile = life::structure_profile(&ctx, 35832).expect("Astrahus profile");
    let st = ctx.db.with_snapshot(|s| s.structures.get(&st_id).cloned().expect("structure"));
    assert_eq!(st.state, life::STATE_ANCHOR_VULNERABLE);
    assert_eq!(st.shield_hp, 0.0);
    assert_eq!(st.armor_hp, 0.0);
    assert_eq!(st.hull_hp, profile.hull_hp);

    // Pause while taking >= 10% of damage cap during AnchorVulnerable
    let before_until = st.reinforced_until_unix;
    let dmg_pause = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let out = life::apply_structure_damage(&ctx, snap, &mut ext, st_id, 600.0, 1.0, OTHER_PILOT, ctx.now_unix()).expect("damage");
        save_ext_state(snap, &ext);
        out
    });
    assert!(dmg_pause.repair_paused, "qualifying damage must pause AnchorVulnerable timer");
    assert!(dmg_pause.reinforced_until_unix > before_until);

    // Killable without timers during AnchorVulnerable
    let doomed_id = 770_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(doomed_id, UpwellStructureRecord {
            structure_id: doomed_id, type_id: 35832, owner_corp_id: st.owner_corp_id,
            solar_system_id: JITA, name: "Doomed Anchor".into(), position: [0.0, 0.0, 0.0],
            state: life::STATE_ANCHOR_VULNERABLE.into(), quantum_core_installed: false, fuel_blocks: 0,
            shield_hp: 0.0, armor_hp: 0.0, hull_hp: 100.0, reinforced_until_unix: ctx.now_unix() + 900,
            last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        let mut ext = load_ext_state(snap);
        let out = life::apply_structure_damage(&ctx, snap, &mut ext, doomed_id, 500.0, 1.0, OTHER_PILOT, ctx.now_unix()).expect("kill");
        save_ext_state(snap, &ext);
        assert_eq!(out.state, life::STATE_DESTROYED);
        assert!(out.destruction.is_some(), "structure in AnchorVulnerable must die without reinforcement timers");
    });

    // 2. Advance past AnchorVulnerable -> Anchoring (24 h invulnerable)
    ctx.advance_secs(life::ANCHOR_VULNERABLE_SECS + 10);
    rpc(&ctx, &mut sess, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await.expect("sync");
    let st = ctx.db.with_snapshot(|s| s.structures.get(&st_id).cloned().expect("structure"));
    assert_eq!(st.state, life::STATE_ANCHORING);
    let invuln_err = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        life::apply_structure_damage(&ctx, snap, &mut ext, st_id, 500.0, 1.0, OTHER_PILOT, ctx.now_unix())
    }).expect_err("Anchoring must be invulnerable");
    assert_eq!(invuln_err.code, "StructureReinforcedInvulnerable");

    // 3. Advance 24 h -> NoCore; service module cannot be brought online without Quantum Core
    ctx.advance_secs(life::ANCHORING_SECS + 1);
    rpc(&ctx, &mut sess, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await.expect("sync");
    let st = ctx.db.with_snapshot(|s| s.structures.get(&st_id).cloned().expect("structure"));
    assert_eq!(st.state, life::STATE_NO_CORE);

    let srv_id = grant(&ctx, st.owner_corp_id, st_id, 35894, InventoryFlag::ServiceSlot0, 1);
    let online_no_core = rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(st_id), PyRep::Integer(srv_id)]).await;
    assert_eq!(online_no_core.expect_err("service module must not online without Quantum Core").code, "QuantumCoreRequired");

    // Wrong-hull Quantum Core (Fortizar core in Astrahus) must NOT install
    let fortizar_core = life::structure_profile(&ctx, 35833).expect("Fortizar").core_type_id.expect("Fortizar core");
    let wrong_core_item = grant(&ctx, st.owner_corp_id, st_id, fortizar_core, InventoryFlag::StructureDeedBay, 1);
    rpc(&ctx, &mut sess, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await.expect("sync");
    let st_still_nocore = ctx.db.with_snapshot(|s| s.structures.get(&st_id).cloned().expect("structure"));
    assert_eq!(st_still_nocore.state, life::STATE_NO_CORE);
    assert!(!st_still_nocore.quantum_core_installed);
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&wrong_core_item); });

    // 4. Install matching Astrahus Quantum Core (56201) -> Onlining (15 min)
    let core_type = profile.core_type_id.expect("Astrahus core type");
    grant(&ctx, st.owner_corp_id, st_id, core_type, InventoryFlag::StructureDeedBay, 1);
    rpc(&ctx, &mut sess, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await.expect("sync");
    let st = ctx.db.with_snapshot(|s| s.structures.get(&st_id).cloned().expect("structure"));
    assert_eq!(st.state, life::STATE_ONLINING);
    assert!(st.quantum_core_installed);

    // Pause while taking >= 10% of damage cap during Onlining
    let onlining_until = st.reinforced_until_unix;
    let online_pause = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let out = life::apply_structure_damage(&ctx, snap, &mut ext, st_id, 600.0, 1.0, OTHER_PILOT, ctx.now_unix()).expect("damage");
        save_ext_state(snap, &ext);
        out
    });
    assert!(online_pause.repair_paused, "qualifying damage must pause Onlining timer");
    assert!(online_pause.reinforced_until_unix > onlining_until);

    // Killable without timers during Onlining
    let doomed_online_id = 770_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(doomed_online_id, UpwellStructureRecord {
            structure_id: doomed_online_id, type_id: 35832, owner_corp_id: st.owner_corp_id,
            solar_system_id: JITA, name: "Doomed Onlining".into(), position: [0.0, 0.0, 0.0],
            state: life::STATE_ONLINING.into(), quantum_core_installed: true, fuel_blocks: 100,
            shield_hp: 0.0, armor_hp: 0.0, hull_hp: 100.0, reinforced_until_unix: ctx.now_unix() + 900,
            last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        let mut ext = load_ext_state(snap);
        let out = life::apply_structure_damage(&ctx, snap, &mut ext, doomed_online_id, 500.0, 1.0, OTHER_PILOT, ctx.now_unix()).expect("kill");
        save_ext_state(snap, &ext);
        assert_eq!(out.state, life::STATE_DESTROYED);
        assert!(out.destruction.is_some(), "structure in Onlining must die without reinforcement timers");
    });

    // Advance past Onlining -> ShieldVulnerable (structure, armor, and shield fully healed)
    ctx.advance_secs(life::ONLINING_SECS + 10);
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

#[tokio::test]
async fn test_fitting_state_locks_and_pochven_anchoring() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = pilot_in_space(&ctx, 3, OWNER, "Fit Lock Corp", "FLCK").await;

    let st_id = extract_id(&anchor_upwell(&ctx, &mut sess, 35832, "Astrahus Fit").await.expect("Anchor"));
    let profile = life::structure_profile(&ctx, 35832).expect("profile");
    let mod_id = grant(&ctx, OWNER, st_id, 35894, InventoryFlag::Hangar, 1);

    // Refit refused while AnchorVulnerable -> StructureFittingLockedInState
    let err_anchor = rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(mod_id), PyRep::Integer(st_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await
        .expect_err("refit must be refused while AnchorVulnerable");
    assert_eq!(err_anchor.code, "StructureFittingLockedInState");

    // Refit refused while Onlining -> StructureFittingLockedInState
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&st_id) {
            st.state = life::STATE_ONLINING.into();
        }
    });
    let err_onlining = rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(mod_id), PyRep::Integer(st_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await
        .expect_err("refit must be refused while Onlining");
    assert_eq!(err_onlining.code, "StructureFittingLockedInState");

    // Refit refused while Reinforced -> StructureFittingLockedInState
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&st_id) {
            st.state = life::STATE_ARMOR_REINFORCED.into();
        }
    });
    let err_reinf = rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(mod_id), PyRep::Integer(st_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await
        .expect_err("refit must be refused while Reinforced");
    assert_eq!(err_reinf.code, "StructureFittingLockedInState");

    // Refit refused while Repairing -> StructureRepairingCannotRefit
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&st_id) {
            st.state = life::STATE_SHIELD_VULNERABLE.into();
            st.shield_hp = profile.shield_hp;
            st.armor_hp = profile.armor_hp;
            st.hull_hp = profile.hull_hp;
        }
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(st_id).or_default().repair_until_unix = Some(ctx.now_unix() + 600);
        save_ext_state(snap, &ext);
    });
    let err_repairing = rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(mod_id), PyRep::Integer(st_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await
        .expect_err("refit must be refused while repairing");
    assert_eq!(err_repairing.code, "StructureRepairingCannotRefit");

    // Refit refused while damaged / under attack -> StructureDamagedCannotRefit
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(st_id).or_default().repair_until_unix = None;
        save_ext_state(snap, &ext);
        if let Some(st) = snap.structures.get_mut(&st_id) {
            st.shield_hp = profile.shield_hp * 0.85;
        }
    });
    let err_damaged = rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(mod_id), PyRep::Integer(st_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await
        .expect_err("refit must be refused when shields < 100%");
    assert_eq!(err_damaged.code, "StructureDamagedCannotRefit");

    // Transition to ShieldVulnerable with full shields -> refit succeeds
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&st_id) {
            st.shield_hp = profile.shield_hp;
        }
    });
    let ok_refit = rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(mod_id), PyRep::Integer(st_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await;
    assert!(ok_refit.is_ok(), "refit must succeed at full shields in ShieldVulnerable: {ok_refit:?}");

    // Pochven anchoring restrictions: Upwell refused, existing ones remain, Customs Office requires >= 1.5 Triglavian standing
    let pochven_sys = ctx.sde.solar_systems.values().find(|s| s.region_id == life::POCHVEN_REGION_ID).map(|s| s.solar_system_id).expect("Pochven system");
    let existing_poch_id = 770_099;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(existing_poch_id, UpwellStructureRecord {
            structure_id: existing_poch_id, type_id: 35832, owner_corp_id: 98_000_001,
            solar_system_id: pochven_sys, name: "Existing Pochven Astrahus".into(), position: [0.0, 0.0, 0.0],
            state: life::STATE_SHIELD_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 100,
            shield_hp: profile.shield_hp, armor_hp: profile.armor_hp, hull_hp: profile.hull_hp,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
    });

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

    // Existing Pochven Upwell structure remains intact
    life::sync_all_structures(&ctx);
    let poch_info = rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(existing_poch_id)]).await;
    assert!(poch_info.is_ok(), "existing Upwell structure in Pochven must remain: {poch_info:?}");

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

#[tokio::test]
async fn test_reinforce_hour_hack_reveal_and_ewar_immunity() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = pilot_in_space(&ctx, 4, OWNER, "Hack Owner Corp", "HACK").await;
    let st_id = extract_id(&anchor_upwell(&ctx, &mut sess, 35832, "Astrahus Hack").await.expect("Anchor"));

    // Spawn structure ball & hacker ship ball in Jita
    let st_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.structures.get(&st_id).expect("st").position));
    let jita_sys = ctx.systems.get_or_spawn(JITA);
    let _ = jita_sys.spawn_ball(Ball {
        id: st_id, type_id: 35832, position: st_pos, velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1.0e9, radius: 1_000.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OTHER_PILOT) {
            ch.user_id = 1;
        }
    });
    let mut hacker_sess = SessionState::new(5);
    rpc(&ctx, &mut hacker_sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.expect("Login");
    rpc(&ctx, &mut hacker_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OTHER_PILOT)]).await.expect("Select");
    ctx.advance_secs(12);
    rpc(&ctx, &mut hacker_sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    let hacker_ship = hacker_sess.ship_id.expect("hacker_ship");
    jita_sys.set_ball_position(hacker_ship, st_pos + DVec3::new(900.0, 0.0, 0.0)).await;
    let _ = jita_sys.add_target_lock(hacker_ship, eve_destiny::TargetLock {
        target_id: st_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None,
    }).await;

    // EWAR immunity: activate ECM, dampener, weapon disruptor, target painter, web, warp scrambler, and energy neutralizer
    let ewar_groups = [201u32, 208, 291, 379, 65, 52, 71];
    for (idx, grp) in ewar_groups.into_iter().enumerate() {
        let mod_tid = ctx.sde.types.values().find(|t| t.group_id == grp).map(|t| t.type_id).unwrap_or_else(|| panic!("missing EWAR group {grp}"));
        let flag = InventoryFlag::from_u16(19 + idx as u16);
        let mod_id = grant(&ctx, OTHER_PILOT, hacker_ship, mod_tid, flag, 1);
        let act = rpc(&ctx, &mut hacker_sess, "dogmaIM", "Activate", &[PyRep::Integer(mod_id), PyRep::String("default".into()), PyRep::Integer(st_id)]).await
            .unwrap_or_else(|e| panic!("EWAR group {grp} activate failed: {e:?}"));
        assert_eq!(act.dict_get("immune"), Some(&PyRep::Bool(true)), "structure must be immune to EWAR group {grp}");
        assert_eq!(act.dict_get("ewarAssistRecorded"), Some(&PyRep::Bool(false)), "EWAR group {grp} must have no effect");
    }

    // Non-member cannot see reinforce hour before hack
    let before = rpc(&ctx, &mut hacker_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.expect("GetStructureInfo before");
    assert_eq!(before.dict_get("reinforceHour"), Some(&PyRep::None), "reinforceHour must be hidden before hack");

    // Data Analyzer hack reveals reinforce hour to hacker
    let _data_mod = grant(&ctx, OTHER_PILOT, hacker_ship, 22175, InventoryFlag::MedSlot7, 1);
    let start = rpc(&ctx, &mut hacker_sess, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(st_id)]).await.expect("StartNewGameInstance");
    assert_eq!(start.dict_get("status").and_then(|v| v.as_str()), Some("InProgress"));
    let mut won = false;
    for _pass in 0..8 {
        for tile in 1..24 {
            for _ in 0..5 {
                if let Ok(step) = rpc(&ctx, &mut hacker_sess, "hackingMgr", "ClickedOnTile", &[PyRep::Integer(st_id), PyRep::Integer(tile)]).await {
                    if step.dict_get("status").and_then(|v| v.as_str()) == Some("Won") {
                        won = true;
                        break;
                    }
                } else {
                    break;
                }
            }
            if won { break; }
        }
        if won { break; }
    }
    assert!(won, "Data Analyzer hack must complete with Won");

    let after = rpc(&ctx, &mut hacker_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.expect("GetStructureInfo after");
    assert!(after.dict_get("reinforceHour").and_then(|v| v.as_i64()).is_some(), "reinforceHour must be revealed after hack");
}
