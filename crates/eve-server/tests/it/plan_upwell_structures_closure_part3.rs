//! Upwell Structure Anchoring, Quantum Core, Pochven, Palatine Keepstar, Tethering & Docking Lock
//! parity tests (`plan_upwell_structures_closure_part3.rs`).

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::space_destiny::space_nav_travel::sync_tether;
use eve_server::services::station_structure::structure_lifecycle as life;
use glam::DVec3;
use std::collections::HashMap;

const OWNER: i64 = 90_000_001;
const OUTSIDER: i64 = 90_000_003;
const RIVAL_CORP: i64 = 98_999_999;
const JITA: u32 = 30_000_142;
const RANCER: u32 = 30_002_718;
const KINO_POCHVEN: u32 = 30_000_157;

fn prepare_siege(ctx: &ServerContext, id: i64, charge_dmg_em: f64) {
    ctx.db.with_mut_snapshot(|snap| {
        let my_corp = snap.characters.get(&OWNER).map(|c| c.corporation_id).expect("corp");
        if let Some(s) = snap.structures.get_mut(&id) {
            s.owner_corp_id = RIVAL_CORP;
        }
        snap.wars.insert(
            77,
            eve_db::WarRecord {
                war_id: 77,
                aggressor_corp_id: my_corp,
                defender_corp_id: RIVAL_CORP,
                mutual: true,
                started_unix: 0,
                active: true,
            },
        );
        if let Some(w) = snap.items.get_mut(&100_000_005) {
            w.location_id = 100_000_001;
            w.flag = InventoryFlag::HiSlot0;
            w.singleton = true;
            if charge_dmg_em > 0.0 {
                w.dynamic_attributes.insert(64, 5_000.0);
            } else {
                w.dynamic_attributes.remove(&64);
            }
        }
        let old_ammo: Vec<i64> = snap.items.values().filter(|i| i.location_id == 100_000_001 && i.flag == InventoryFlag::HiSlot0 && i.item_id != 100_000_005).map(|i| i.item_id).collect();
        for aid in old_ammo { snap.items.remove(&aid); }
    });
    let _ = grant(ctx, OWNER, 100_000_001, 185, InventoryFlag::HiSlot0, 10_000);
    ctx.invalidate_ship_fitting(100_000_001);
}

async fn fire_weapon(ctx: &ServerContext, sess: &mut SessionState, target_id: i64) -> Result<PyRep, BridgeRpcError> {
    let (sys_id, ship_id) = (sess.solar_system_id.expect("sys"), sess.ship_id.expect("ship"));
    let sys = ctx.systems.get_or_spawn(sys_id);
    if !sys.get_target_locks(ship_id).await.unwrap_or_default().iter().any(|l| l.target_id == target_id && l.locked) {
        rpc(ctx, sess, "dogmaIM", "AddTarget", &[PyRep::Integer(target_id)]).await?;
        let _ = sys.step_manual(12.0).await;
    }
    rpc(ctx, sess, "dogmaIM", "Activate", &[PyRep::Integer(100_000_005), PyRep::String("projectileFired".into()), PyRep::Integer(target_id)]).await
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 { rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or(0) }
fn kv_str(rep: &PyRep, key: &str) -> String { rep.dict_get(key).and_then(|v| v.as_str()).unwrap_or("").to_string() }

fn grant(ctx: &ServerContext, owner: i64, location: i64, type_id: u32, flag: InventoryFlag, qty: i32) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(id, InventoryItemRecord { item_id: id, type_id, owner_id: owner, location_id: location, flag, quantity: qty, singleton: qty == 1, version: 1, ..Default::default() });
        id
    })
}

fn attr(ctx: &ServerContext, type_id: u32, attr_id: u16) -> f64 {
    ctx.sde.types.get(&type_id).and_then(|t| t.default_attributes.get(&attr_id).copied()).unwrap_or_else(|| panic!("SDE type {type_id} lacks attribute {attr_id}"))
}

fn new_test_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

async fn pilot_in_space(ctx: &ServerContext, sess_id: u64) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)]).await.expect("Select");
    ctx.advance_secs(12);
    rpc(ctx, &mut sess, "corpRegistry", "AddCorporation", &[PyRep::String("Upwell Part3 Corp".into()), PyRep::String("UPW3".into())]).await.expect("AddCorporation");
    ctx.advance_secs(12);
    rpc(ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    sess
}

async fn move_ship(ctx: &ServerContext, sess: &SessionState, pos: DVec3) {
    assert!(ctx.systems.get_or_spawn(sess.solar_system_id.expect("sys")).set_ball_position(sess.ship_id.expect("ship"), pos).await);
}

fn open_space(ctx: &ServerContext, sys_id: u32, k: f64) -> DVec3 {
    let base = ctx.sde.stations.values().find(|s| s.solar_system_id == sys_id).map(|s| s.position).unwrap_or(DVec3::ZERO);
    base + DVec3::new(0.0, 5.0e9 + k * 3.0e9, 0.0)
}

async fn anchor(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, name: &str) -> Result<PyRep, BridgeRpcError> {
    let ship = sess.ship_id.expect("ship");
    grant(ctx, OWNER, ship, type_id, InventoryFlag::Cargo, 1);
    rpc(ctx, sess, "structureDeployment", "Anchor", &[PyRep::Integer(i64::from(type_id)), PyRep::String(name.into())]).await
}

async fn observe(ctx: &ServerContext, sess: &mut SessionState, id: i64) -> Option<UpwellStructureRecord> {
    let _ = rpc(ctx, sess, "structureGuests", "GetGuests", &[PyRep::Integer(id)]).await;
    ctx.db.with_snapshot(|snap| snap.structures.get(&id).cloned())
}

async fn online_structure(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, name: &str) -> i64 {
    let id = kv_i64(&anchor(ctx, sess, type_id, name).await.expect("Anchor"), "structureID");
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS);
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_NO_CORE);
    let core = attr(ctx, type_id, life::ATTR_REQUIRES_DEED_TYPE) as u32;
    rpc(ctx, sess, "structureDocking", "Dock", &[PyRep::Integer(id)]).await.expect("Dock");
    let core_item = grant(ctx, OWNER, sess.ship_id.expect("ship"), core, InventoryFlag::Cargo, 1);
    rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(core_item), PyRep::Integer(id), PyRep::Integer(172)]).await.expect("move core");
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_ONLINING);
    ctx.advance_virtual_clock(life::ONLINING_SECS);
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_SHIELD_VULNERABLE);
    id
}

#[tokio::test]
async fn test_upwell_anchoring_quantum_core_pochven_and_palatine() {
    // parity: feat:structures.anchoring-onlining-phases, feat:structures.quantum-core-gate, feat:structures.pochven-anchoring-restrictions, feat:structures.upwell-palatine-keepstar-limit
    eve_testkit::attest(&[
        "feat:structures.anchoring-onlining-phases",
        "feat:structures.quantum-core-gate",
        "feat:structures.pochven-anchoring-restrictions",
        "feat:structures.upwell-palatine-keepstar-limit",
    ]);

    let ctx = new_test_ctx();
    let mut sess = pilot_in_space(&ctx, 7102).await;
    let my_corp = ctx.db.with_snapshot(|snap| snap.characters.get(&OWNER).unwrap().corporation_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(OWNER).or_default().insert(3416, eve_db::TrainedSkillRecord { type_id: 3416, level: 5, skill_points: 256_000.0 });
    });

    // 1. Minimum 1,000 km anchoring distance check + 4-phase anchoring/quantum-core/onlining progression via live weapon RPCs.
    let station_pos = ctx.sde.stations.values().find(|s| s.solar_system_id == JITA).unwrap().position;
    move_ship(&ctx, &sess, station_pos + DVec3::new(500_000.0, 0.0, 0.0)).await;
    assert_eq!(anchor(&ctx, &mut sess, 35832, "Too Close").await.expect_err("too close").code, "TooCloseToStructureOrStation");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, it| !(it.location_id == sess.ship_id.unwrap() && it.type_id == 35832 && it.flag == InventoryFlag::Cargo));
    });

    let pos3 = open_space(&ctx, JITA, 3.0);
    move_ship(&ctx, &sess, pos3).await;
    let st_id = kv_i64(&anchor(&ctx, &mut sess, 35832, "Phased Astrahus").await.expect("Anchor"), "structureID");
    let st_anchored = observe(&ctx, &mut sess, st_id).await.unwrap();
    assert_eq!((st_anchored.state.as_str(), st_anchored.shield_hp, st_anchored.armor_hp), (life::STATE_ANCHOR_VULNERABLE, 0.0, 0.0));
    let prof_hp = life::structure_profile(&ctx, 35832).unwrap();

    // Pre-NoCore docking rejected on BOTH structureDocking.Dock and beyonce.CmdDock:
    assert_eq!(rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(st_id)]).await.expect_err("dock AnchorVulnerable").code, "StructureNotDockable");
    assert_eq!(rpc(&ctx, &mut sess, "beyonce", "CmdDock", &[PyRep::Integer(st_id)]).await.expect_err("cmddock AnchorVulnerable").code, "StructureNotDockable");

    // AnchorVulnerable pause under qualifying live weapon fire and timerless destruction:
    let t_init = st_anchored.reinforced_until_unix;
    prepare_siege(&ctx, st_id, 0.0);
    fire_weapon(&ctx, &mut sess, st_id).await.expect("sub-threshold anchor hit");
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&st_id].reinforced_until_unix), t_init, "sub-threshold hit does not pause anchoring");
    prepare_siege(&ctx, st_id, 20_000.0);
    fire_weapon(&ctx, &mut sess, st_id).await.expect("qualifying anchor hit");
    assert!(ctx.db.with_snapshot(|s| s.structures[&st_id].reinforced_until_unix) > t_init, "qualifying weapon DPS pauses AnchorVulnerable timer");
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&st_id).unwrap().owner_corp_id = my_corp);

    let pos4 = open_space(&ctx, JITA, 4.0);
    move_ship(&ctx, &sess, pos4).await;
    let doomed_id = kv_i64(&anchor(&ctx, &mut sess, 35832, "Doomed Anchor").await.expect("anchor doomed"), "structureID");
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&doomed_id).unwrap().hull_hp = 10.0);
    prepare_siege(&ctx, doomed_id, 20_000.0);
    fire_weapon(&ctx, &mut sess, doomed_id).await.expect("destroy in AnchorVulnerable");
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&doomed_id].state.clone()), life::STATE_DESTROYED);

    move_ship(&ctx, &sess, pos3).await;
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + 30);
    assert_eq!(observe(&ctx, &mut sess, st_id).await.unwrap().state, life::STATE_ANCHORING);
    assert_eq!(rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(st_id)]).await.expect_err("dock Anchoring").code, "StructureNotDockable");
    assert_eq!(rpc(&ctx, &mut sess, "beyonce", "CmdDock", &[PyRep::Integer(st_id)]).await.expect_err("cmddock Anchoring").code, "StructureNotDockable");
    prepare_siege(&ctx, st_id, 20_000.0);
    assert_eq!(fire_weapon(&ctx, &mut sess, st_id).await.expect_err("invuln during Anchoring").code, "StructureReinforcedInvulnerable");
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&st_id).unwrap().owner_corp_id = my_corp);

    ctx.advance_virtual_clock(life::ANCHORING_SECS);
    let st_nocore = observe(&ctx, &mut sess, st_id).await.unwrap();
    assert_eq!(st_nocore.state, life::STATE_NO_CORE);
    assert!(!st_nocore.quantum_core_installed);

    // Quantum core gate: onlining a service module AND docked ship fitting while quantum_core_installed == false or Onlining are blocked.
    for (hull, core) in [(35832, 56201), (35835, 56202), (35825, 56203), (35833, 56204), (35836, 56205), (35826, 56206), (35834, 56207), (35827, 56208)] {
        assert_eq!(life::structure_profile(&ctx, hull).and_then(|p| p.core_type_id), Some(core));
    }
    ctx.advance_virtual_clock(65);
    move_ship(&ctx, &sess, DVec3::from_array(st_nocore.position) + DVec3::new(500.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(st_id)]).await.expect("Dock");
    let active_ship = sess.ship_id.unwrap();
    let ship_mod_cargo = grant(&ctx, OWNER, active_ship, 10039, InventoryFlag::Cargo, 1);
    let ship_mod_fit = grant(&ctx, OWNER, active_ship, 10039, InventoryFlag::MedSlot0, 1);
    let _hangar_mod = grant(&ctx, OWNER, st_id, 10039, InventoryFlag::Hangar, 1);
    ctx.db.with_mut_snapshot(|snap| {
        let ship_tid = snap.items[&active_ship].type_id;
        snap.saved_fittings.insert(9001, eve_db::SavedFittingRecord {
            fitting_id: 9001, owner_id: OWNER, ship_type_id: ship_tid,
            name: "NoCore Test Fit".into(), description: String::new(),
            items: vec![(10039, InventoryFlag::MedSlot1, 1)],
        });
    });
    assert_eq!(rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(ship_mod_cargo), PyRep::Integer(active_ship), PyRep::Integer(InventoryFlag::MedSlot1 as i64)]).await.expect_err("ship fit NoCore").code, "QuantumCoreRequired");
    assert_eq!(rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(ship_mod_fit), PyRep::Integer(active_ship), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect_err("ship unfit NoCore").code, "QuantumCoreRequired");
    assert_eq!(rpc(&ctx, &mut sess, "invbroker", "MultiAdd", &[PyRep::List(vec![PyRep::Integer(ship_mod_fit)]), PyRep::Integer(active_ship), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect_err("multiadd unfit NoCore").code, "QuantumCoreRequired");
    assert_eq!(rpc(&ctx, &mut sess, "ship", "FitShips", &[PyRep::List(vec![PyRep::Integer(active_ship)]), PyRep::Integer(9001)]).await.expect_err("FitShips NoCore").code, "QuantumCoreRequired");
    assert_eq!(rpc(&ctx, &mut sess, "dogmaIM", "StripFitting", &[PyRep::Integer(active_ship)]).await.expect_err("StripFitting NoCore").code, "QuantumCoreRequired");

    let clone_bay_mod = grant(&ctx, OWNER, active_ship, 35894, InventoryFlag::Cargo, 1);
    let st_fuel = grant(&ctx, OWNER, active_ship, 4247, InventoryFlag::Cargo, 5_000);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(st_fuel), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(5_000)]).await.expect("fuel");
    assert_eq!(rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(clone_bay_mod), PyRep::Integer(st_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await.expect_err("no fit in NoCore").code, "StructureFittingLockedInState");
    ctx.db.with_mut_snapshot(|snap| {
        let m = snap.items.get_mut(&clone_bay_mod).unwrap();
        m.owner_id = my_corp; m.location_id = st_id; m.flag = InventoryFlag::ServiceSlot0;
        snap.module_online_states.insert(clone_bay_mod, false);
    });
    assert_eq!(rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(st_id), PyRep::Integer(clone_bay_mod)]).await.expect_err("no core").code, "QuantumCoreRequired");

    // Negative 1: wrong core type (56202) into StructureDeedBay (172) fails with InvalidItemType
    let wrong_core = grant(&ctx, OWNER, active_ship, 56202, InventoryFlag::Cargo, 1);
    assert_eq!(
        rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(wrong_core), PyRep::Integer(st_id), PyRep::Integer(172)]).await.expect_err("wrong core type").code,
        "InvalidItemType"
    );
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&wrong_core); });

    // Negative 2: moving quantum core (56201) to Hangar (flag 4) leaves structure in NoCore and core is NOT consumed
    let hangar_core = grant(&ctx, OWNER, active_ship, 56201, InventoryFlag::Cargo, 1);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(hangar_core), PyRep::Integer(st_id), PyRep::Integer(4)]).await.expect("add core to hangar");
    assert_eq!(observe(&ctx, &mut sess, st_id).await.unwrap().state, life::STATE_NO_CORE);
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&hangar_core).expect("hangar core item must exist");
        assert_eq!(it.flag, InventoryFlag::Hangar);
        assert_eq!(it.quantity, 1);
    });

    // Positive: matching core (56201) deposited into StructureDeedBay (172) enters Onlining (15m) where ship fitting remains blocked.
    let right_core = grant(&ctx, OWNER, active_ship, 56201, InventoryFlag::Cargo, 1);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(right_core), PyRep::Integer(st_id), PyRep::Integer(172)]).await.expect("add right core");
    assert_eq!(observe(&ctx, &mut sess, st_id).await.unwrap().state, life::STATE_ONLINING);
    assert_eq!(rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(ship_mod_cargo), PyRep::Integer(active_ship), PyRep::Integer(InventoryFlag::MedSlot1 as i64)]).await.expect_err("ship fit Onlining").code, "QuantumCoreRequired");
    assert_eq!(rpc(&ctx, &mut sess, "ship", "FitShips", &[PyRep::List(vec![PyRep::Integer(active_ship)]), PyRep::Integer(9001)]).await.expect_err("FitShips Onlining").code, "QuantumCoreRequired");
    assert_eq!(rpc(&ctx, &mut sess, "dogmaIM", "StripFitting", &[PyRep::Integer(active_ship)]).await.expect_err("StripFitting Onlining").code, "QuantumCoreRequired");
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock for onlining combat check");

    // Qualifying weapon fire during Onlining pauses timer & damages hull; destroying during Onlining is timerless and drops quantum core ball!
    let t_on0 = ctx.db.with_snapshot(|s| s.structures[&st_id].reinforced_until_unix);
    prepare_siege(&ctx, st_id, 20_000.0);
    fire_weapon(&ctx, &mut sess, st_id).await.expect("onlining qualifying hit");
    ctx.db.with_mut_snapshot(|snap| {
        assert!(snap.structures[&st_id].reinforced_until_unix > t_on0);
        assert!(snap.structures[&st_id].hull_hp < prof_hp.hull_hp);
        snap.structures.get_mut(&st_id).unwrap().owner_corp_id = my_corp;
    });

    let pos5 = open_space(&ctx, JITA, 5.0);
    move_ship(&ctx, &sess, pos5).await;
    let doomed_on = kv_i64(&anchor(&ctx, &mut sess, 35832, "Doomed Onlining").await.expect("anchor doomed on"), "structureID");
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS);
    let _ = observe(&ctx, &mut sess, doomed_on).await;
    ctx.advance_virtual_clock(65);
    rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(doomed_on)]).await.expect("Dock doomed on");
    let core_doomed = grant(&ctx, OWNER, sess.ship_id.unwrap(), 56201, InventoryFlag::Cargo, 1);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(core_doomed), PyRep::Integer(doomed_on), PyRep::Integer(172)]).await.expect("core doomed");
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock doomed");
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&doomed_on).unwrap().hull_hp = 10.0);
    prepare_siege(&ctx, doomed_on, 20_000.0);
    fire_weapon(&ctx, &mut sess, doomed_on).await.expect("destroy during Onlining");
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&doomed_on].state.clone()), life::STATE_DESTROYED);
    let (wreck_id, dropped_core_id) = ctx.db.with_snapshot(|snap| {
        let p = snap.structures[&doomed_on].position;
        let wreck = snap.items.values().find(|i| {
            i.location_id == JITA as i64
                && snap.item_positions.get(&i.item_id) == Some(&p)
                && (i.type_id == 26557 || ctx.sde.wrecks.values().any(|&w| w == i.type_id))
        }).expect("structure wreck container");
        let core = snap.items.values().find(|i| i.type_id == 56201 && i.location_id == wreck.item_id).expect("dropped core item inside wreck");
        (wreck.item_id, core.item_id)
    });
    assert!(ctx.systems.get_or_spawn(JITA).get_ball_snapshot(wreck_id).await.unwrap().is_some(), "structure wreck ball spawned in space");
    assert!(ctx.systems.get_or_spawn(JITA).get_ball_snapshot(dropped_core_id).await.unwrap().is_none(), "core inside wreck has no independent space ball");

    ctx.advance_virtual_clock(life::ONLINING_SECS + 65);
    move_ship(&ctx, &sess, pos3).await;
    let st_online = observe(&ctx, &mut sess, st_id).await.unwrap();
    assert_eq!(st_online.state, life::STATE_SHIELD_VULNERABLE);
    assert!((st_online.hull_hp - prof_hp.hull_hp).abs() < 1.0 && st_online.quantum_core_installed);
    rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(st_id)]).await.expect("Dock online");
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(ship_mod_cargo), PyRep::Integer(active_ship), PyRep::Integer(InventoryFlag::MedSlot1 as i64)]).await.expect("ship fit online with core");
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(ship_mod_fit), PyRep::Integer(active_ship), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect("ship unfit online with core");
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(ship_mod_cargo), PyRep::Integer(active_ship), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect("restore ship_mod_cargo");
    rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(st_id), PyRep::Integer(clone_bay_mod)]).await.expect("online service with core");

    // High-Sec alliance-level war precheck (active alliance war permitted; un-warmed non-mutual war rejected):
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock for alliance war check");
    move_ship(&ctx, &sess, pos3).await;
    let (ally_a, ally_b) = (99_100_001i64, 99_100_002i64);
    prepare_siege(&ctx, st_id, 0.0);
    let now_war = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.clear();
        snap.corporations.get_mut(&my_corp).unwrap().alliance_id = Some(ally_a);
        let mut rcorp = snap.corporations.get(&my_corp).unwrap().clone();
        rcorp.corporation_id = RIVAL_CORP; rcorp.alliance_id = Some(ally_b);
        snap.corporations.insert(RIVAL_CORP, rcorp);
        snap.characters.get_mut(&OWNER).unwrap().alliance_id = Some(ally_a);
        snap.wars.insert(88, eve_db::WarRecord { war_id: 88, aggressor_corp_id: ally_a, defender_corp_id: ally_b, mutual: false, started_unix: now_war, active: true });
    });
    assert_eq!(fire_weapon(&ctx, &mut sess, st_id).await.expect_err("24h warmup").code, "IllegalHighSecStructureAttack");
    ctx.db.with_mut_snapshot(|snap| snap.wars.get_mut(&88).unwrap().started_unix = 0);
    fire_weapon(&ctx, &mut sess, st_id).await.expect("active High-Sec alliance war permitted");
    ctx.db.with_mut_snapshot(|snap| {
        assert!(eve_server::services::structure_acl_defense_rules::are_corps_at_war_in_snap(snap, my_corp, RIVAL_CORP, ctx.now_unix()));
        assert!(eve_server::services::structure_acl_defense_rules::evaluate_auto_defense_target(&ctx, snap, st_id, OWNER));
        snap.structures.get_mut(&st_id).unwrap().owner_corp_id = my_corp;
    });

    // 2. Pochven anchoring restrictions: Upwell structures & POS towers rejected; Mobile Depot allowed; Customs Office gated by >= 1.5 Triglavian standing.
    sess.solar_system_id = Some(KINO_POCHVEN);
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&OWNER).unwrap().solar_system_id = KINO_POCHVEN);
    let poch_ship = sess.ship_id.unwrap();
    let planet_pos = DVec3::new(27660817438.0, -327544042.0, 66850617032.0);
    let ship_pos = planet_pos + DVec3::new(10_000_000.0, 0.0, 0.0);
    let _ = ctx.systems.get_or_spawn(KINO_POCHVEN).spawn_ball(Ball { id: poch_ship, type_id: 606, position: ship_pos, velocity: DVec3::ZERO, radius: 40.0, mass: 1_000_000.0, mode: MovementMode::Stop, speed_fraction: 0.0, max_velocity: 300.0, inertia: 1.0 }).await;
    assert!(matches!(anchor(&ctx, &mut sess, 35832, "Pochven Citadel").await.expect_err("pochven upwell").code.as_str(), "AnchoringDisallowedInPochven" | "CannotAnchorInPochven"));

    let pos_tower = grant(&ctx, OWNER, poch_ship, 12235, InventoryFlag::Cargo, 1);
    let pos_err = rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(pos_tower), PyRep::Tuple(vec![PyRep::Real(ship_pos.x), PyRep::Real(ship_pos.y), PyRep::Real(ship_pos.z)])]).await.expect_err("pochven pos");
    assert!(matches!(pos_err.code.as_str(), "AnchoringRestrictedInPochven" | "StarbaseRemoved"));

    let mobile_depot = grant(&ctx, OWNER, poch_ship, 33474, InventoryFlag::Cargo, 1);
    assert_eq!(kv_str(&rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(mobile_depot), PyRep::Tuple(vec![PyRep::Real(ship_pos.x), PyRep::Real(ship_pos.y), PyRep::Real(ship_pos.z)])]).await.expect("pochven depot"), "state"), "Anchoring");

    let gantry_item = grant(&ctx, OWNER, poch_ship, 3962, InventoryFlag::Cargo, 1);
    assert_eq!(rpc(&ctx, &mut sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_item)]).await.expect_err("low trig standing").code, "InsufficientTriglavianStanding");
    ctx.db.with_mut_snapshot(|snap| { snap.characters.get_mut(&OWNER).unwrap().standings.insert(500_026, 2.0); });
    assert_eq!(kv_str(&rpc(&ctx, &mut sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_item)]).await.expect("high trig standing"), "state"), "Anchored");

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(999_888, eve_db::UpwellStructureRecord {
            structure_id: 999_888, owner_corp_id: 1000009, solar_system_id: KINO_POCHVEN, type_id: 35832,
            state: life::STATE_SHIELD_VULNERABLE.into(), reinforced_until_unix: 0, shield_hp: 10_000.0, armor_hp: 10_000.0, hull_hp: 10_000.0,
            quantum_core_installed: true, fuel_blocks: 1_000, position: [0.0, 0.0, 0.0], name: "Pochven Legacy".into(), last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
    });
    assert!(life::is_operational(&observe(&ctx, &mut sess, 999_888).await.unwrap().state));

    // 3. Upwell Palatine Keepstar (40340) cluster-wide limit of 1 active (replaced via live combat destruction!).
    sess.solar_system_id = Some(RANCER);
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&OWNER).unwrap().solar_system_id = RANCER);
    let rancer_sys = ctx.systems.get_or_spawn(RANCER);
    let _ = rancer_sys.spawn_ball_with_combat(Ball { id: poch_ship, type_id: 587, position: open_space(&ctx, RANCER, 1.0), velocity: DVec3::ZERO, radius: 40.0, mass: 1_000_000.0, mode: MovementMode::Stop, speed_fraction: 0.0, max_velocity: 300.0, inertia: 1.0 }, ctx.build_type_combat_state(587)).await;
    let pal1 = kv_i64(&anchor(&ctx, &mut sess, 40340, "Palatine One").await.expect("first Palatine"), "structureID");
    move_ship(&ctx, &sess, open_space(&ctx, RANCER, 2.0)).await;
    assert_eq!(anchor(&ctx, &mut sess, 40340, "Palatine Two").await.expect_err("second Palatine").code, "PalatineKeepstarLimitReached");

    move_ship(&ctx, &sess, open_space(&ctx, RANCER, 1.0)).await;
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&pal1).unwrap().hull_hp = 10.0);
    prepare_siege(&ctx, pal1, 20_000.0);
    fire_weapon(&ctx, &mut sess, pal1).await.expect("destroy Palatine One in AnchorVulnerable");
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&pal1].state.clone()), life::STATE_DESTROYED);
    move_ship(&ctx, &sess, open_space(&ctx, RANCER, 2.0)).await;
    assert!(anchor(&ctx, &mut sess, 40340, "Palatine Replacement").await.is_ok());
}

#[tokio::test]
async fn test_upwell_tethering_eligibility_restrictions_and_docking_lock() {
    // parity: feat:structures.tether-eligibility-and-restrictions, feat:structures.tethering-docking-lock
    eve_testkit::attest(&[
        "feat:structures.tether-eligibility-and-restrictions",
        "feat:structures.tethering-docking-lock",
    ]);

    let ctx = new_test_ctx();
    let mut sess = pilot_in_space(&ctx, 7201).await;
    let st_pos = open_space(&ctx, JITA, 1.0);
    move_ship(&ctx, &sess, st_pos).await;
    let astra_id = online_structure(&ctx, &mut sess, 35832, "Tether Astrahus").await;
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");

    let ship_id = sess.ship_id.unwrap();
    let jita_sys = ctx.systems.get_or_spawn(JITA);
    move_ship(&ctx, &sess, st_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;

    {
        let mut cs = jita_sys.get_combat_state(ship_id).await.unwrap().unwrap();
        cs.shield_hp = cs.max_shield_hp * 0.4; cs.armor_hp = cs.max_armor_hp * 0.4;
        cs.structure_hp = cs.max_structure_hp * 0.4; cs.capacitor = cs.max_capacitor * 0.4;
        let _ = jita_sys.set_combat_state(ship_id, cs).await;
    }
    sync_tether(&ctx, OWNER, JITA, ship_id).await;
    let cs_tethered = jita_sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_tethered.is_tethered && cs_tethered.shield_hp > cs_tethered.max_shield_hp * 0.6);

    let hp_before = cs_tethered.shield_hp;
    let _ = jita_sys.apply_damage(ship_id, DamageVector { em: 5_000.0, thermal: 5_000.0, kinetic: 5_000.0, explosive: 5_000.0 }).await;
    assert!((jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp - hp_before).abs() < 1e-3);

    let unpiloted_ship = grant(&ctx, OUTSIDER, i64::from(JITA), 606, InventoryFlag::None, 1);
    let _ = jita_sys.spawn_ball_with_combat(Ball { id: unpiloted_ship, type_id: 606, position: st_pos + DVec3::new(1_200.0, 0.0, 0.0), velocity: DVec3::ZERO, radius: 40.0, mass: 1_000_000.0, mode: MovementMode::Stop, speed_fraction: 0.0, max_velocity: 300.0, inertia: 1.0 }, ctx.build_type_combat_state(606)).await;
    sync_tether(&ctx, 0, JITA, unpiloted_ship).await;
    assert!(!jita_sys.get_combat_state(unpiloted_ship).await.unwrap().unwrap().is_tethered);

    let pod_id = grant(&ctx, OWNER, i64::from(JITA), 670, InventoryFlag::None, 1);
    let _ = jita_sys.spawn_ball_with_combat(Ball { id: pod_id, type_id: 670, position: st_pos + DVec3::new(800.0, 0.0, 0.0), velocity: DVec3::ZERO, radius: 10.0, mass: 10_000.0, mode: MovementMode::Stop, speed_fraction: 0.0, max_velocity: 500.0, inertia: 1.0 }, ctx.build_type_combat_state(670)).await;
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&OWNER).unwrap().active_ship_id = pod_id);
    sync_tether(&ctx, OWNER, JITA, pod_id).await;
    assert!(jita_sys.get_combat_state(pod_id).await.unwrap().unwrap().is_tethered);
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&OWNER).unwrap().active_ship_id = ship_id);

    let mut attacker_sess = SessionState::new(7202);
    attacker_sess.user_id = Some(3); attacker_sess.character_id = Some(OUTSIDER); attacker_sess.corporation_id = Some(1000009); attacker_sess.solar_system_id = Some(JITA); attacker_sess.ship_id = Some(unpiloted_ship);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&OWNER).unwrap().clone();
        ch.character_id = OUTSIDER; ch.user_id = 3; ch.name = "Attacker".into(); ch.corporation_id = 1000009; ch.solar_system_id = JITA; ch.active_ship_id = unpiloted_ship;
        snap.characters.insert(OUTSIDER, ch);
        snap.structure_docking_acl_denied.entry(astra_id).or_default().insert(OUTSIDER);
    });
    sync_tether(&ctx, OUTSIDER, JITA, unpiloted_ship).await;
    assert!(!jita_sys.get_combat_state(unpiloted_ship).await.unwrap().unwrap().is_tethered);
    assert!(matches!(rpc(&ctx, &mut attacker_sess, "structureDocking", "Dock", &[PyRep::Integer(astra_id)]).await.expect_err("acl dock").code.as_str(), "ShipNotDockableAtStructure" | "DockingPermissionDenied" | "StructureDockingAccessDenied"));
    assert_eq!(rpc(&ctx, &mut attacker_sess, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_id)]).await.expect_err("tethered lock").code, "TargetInvulnerable");

    let sb_mod = grant(&ctx, OWNER, ship_id, 4266, InventoryFlag::HiSlot0, 1);
    ctx.invalidate_ship_fitting(ship_id);
    assert_eq!(rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(sb_mod), PyRep::String("empWave".into())]).await.expect_err("sb tethered").code, "CantActivateWhileTethered");
    let ore_item = grant(&ctx, OWNER, ship_id, 1230, InventoryFlag::SpecializedOreHold, 1_000);
    assert_eq!(rpc(&ctx, &mut sess, "inSpaceCompressionMgr", "CompressItemInSpace", &[PyRep::Integer(ore_item)]).await.expect_err("compress tethered").code, "CannotCompressWhileTethered");

    rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(unpiloted_ship)]).await.expect("lock target");
    assert!(!jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
    let _ = jita_sys.clear_target_locks(ship_id).await;
    sync_tether(&ctx, OWNER, JITA, ship_id).await;
    assert!(jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);

    ctx.active_fighters.write().unwrap().insert(999_001, eve_server::router::context::ActiveFighterSquadron {
        ball_id: 999_001, item_id: 999_001, type_id: 23055, owner_char_id: OWNER, parent_ship_id: ship_id, tube_index: 0, squadron_size: 9, target_id: None, mwd_active: false, is_abandoned: false,
    });
    sync_tether(&ctx, OWNER, JITA, ship_id).await;
    assert!(!jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
    ctx.active_fighters.write().unwrap().get_mut(&999_001).unwrap().is_abandoned = true;
    sync_tether(&ctx, OWNER, JITA, ship_id).await;
    assert!(jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
    ctx.active_fighters.write().unwrap().clear();

    for mod_tid in [4292u32, 21096] {
        let m = grant(&ctx, OWNER, ship_id, mod_tid, InventoryFlag::HiSlot1, 1);
        ctx.db.with_mut_snapshot(|snap| { snap.module_active_states.insert(m); });
        sync_tether(&ctx, OWNER, JITA, ship_id).await;
        assert!(!jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
        ctx.db.with_mut_snapshot(|snap| { snap.module_active_states.remove(&m); });
    }

    let mut scram_cs = jita_sys.get_combat_state(ship_id).await.unwrap().unwrap();
    scram_cs.warp_core_strength = 2; scram_cs.warp_scramble_strength = 1;
    let _ = jita_sys.set_combat_state(ship_id, scram_cs.clone()).await;
    sync_tether(&ctx, OWNER, JITA, ship_id).await;
    assert!(jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
    scram_cs.warp_scramble_strength = 3;
    let _ = jita_sys.set_combat_state(ship_id, scram_cs.clone()).await;
    sync_tether(&ctx, OWNER, JITA, ship_id).await;
    assert!(!jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
    assert_eq!(rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(astra_id)]).await.expect_err("scram dock").code, "ShipWarpScrambled");
    scram_cs.warp_scramble_strength = 0; scram_cs.warp_core_strength = 0;
    let _ = jita_sys.set_combat_state(ship_id, scram_cs).await;

    ctx.crimewatch.write().unwrap().trigger_weapon_timer(OWNER, ctx.now_unix());
    sync_tether(&ctx, OWNER, JITA, ship_id).await;
    assert!(!jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
    assert_eq!(rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(astra_id)]).await.expect_err("weapons timer dock").code, "WeaponsTimerActive");
    ctx.advance_virtual_clock(65);

    ctx.crimewatch.write().unwrap().get_or_create(OWNER).pvp_timer_until_unix = ctx.now_unix() + 900;
    sync_tether(&ctx, OWNER, JITA, ship_id).await;
    assert!(!jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
    ctx.crimewatch.write().unwrap().get_or_create(OWNER).pvp_timer_until_unix = 0;

    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&OWNER).unwrap().jump_cooldown_until_unix = ctx.now_unix() + 600);
    sync_tether(&ctx, OWNER, JITA, ship_id).await;
    assert!(!jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&OWNER).unwrap().jump_cooldown_until_unix = 0);

    ctx.db.with_mut_snapshot(|snap| snap.items.get_mut(&ship_id).unwrap().type_id = 11567);
    let _ = jita_sys.spawn_ball_with_combat(Ball { id: ship_id, type_id: 11567, position: st_pos + DVec3::new(1_500.0, 0.0, 0.0), velocity: DVec3::ZERO, radius: 400.0, mass: 1.0e9, mode: MovementMode::Stop, speed_fraction: 0.0, max_velocity: 100.0, inertia: 1.0 }, ctx.build_type_combat_state(11567)).await;
    assert!(matches!(rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(astra_id)]).await.expect_err("titan in astrahus").code.as_str(), "ShipTooLargeForStructure" | "ShipNotDockableAtStructure"));
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&astra_id).unwrap().type_id = 35834);
    assert!(rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(astra_id)]).await.is_ok());
}
