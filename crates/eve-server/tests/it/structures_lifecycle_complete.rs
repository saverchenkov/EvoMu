//! Upwell structure lifecycle tests (`structure_lifecycle` engine + `structureDeployment.Anchor`,
//! `invbroker.Add` flag 133 fuel bay, `dogmaIM.Activate` live weapon siege, `structureDeployment.Unanchor`).
//! Expected values are read from the real SDE (`EVOMU_SDE_PATH`) or from the engine's
//! documented DERIVED timer constants.

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::services::station_structure::structure_upwell_ops::load_ext_state;
use glam::DVec3;
use std::collections::HashMap;

const OWNER: i64 = 90_000_001;
const JITA: u32 = 30_000_144;
const RIVAL_CORP: i64 = 98_999_999;

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
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

/// Logged-in pilot with a player corp, undocked in Perimeter and moved far from stations/gates.
async fn pilot_in_space(ctx: &ServerContext, sess_id: u64) -> SessionState {
    let perim_station = ctx.sde.stations.values().find(|s| s.solar_system_id == JITA).map(|s| s.station_id).expect("Perimeter station");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            let ship_id = ch.active_ship_id;
            ch.solar_system_id = JITA;
            ch.station_id = Some(perim_station);
            if let Some(sh) = snap.items.get_mut(&ship_id) {
                sh.location_id = perim_station;
            }
        }
    });
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)]).await.expect("Select");
    ctx.advance_secs(12);
    rpc(ctx, &mut sess, "corpRegistry", "AddCorporation", &[PyRep::String("Lifecycle Holdings".into()), PyRep::String("LIFE".into())]).await.expect("AddCorporation");
    ctx.advance_virtual_clock(7 * 86_400 + 12);
    rpc(ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    sess
}

async fn move_ship(ctx: &ServerContext, sess: &SessionState, pos: DVec3) {
    assert!(ctx.systems.get_or_spawn(sess.solar_system_id.expect("sys")).set_ball_position(sess.ship_id.expect("ship"), pos).await);
}

/// A point in Jita at least 1,000 km from every station, stargate and belt (offset along +Y).
fn open_space(ctx: &ServerContext, k: f64) -> DVec3 {
    let st = ctx.sde.stations.values().filter(|s| s.solar_system_id == JITA).map(|s| s.position).next().expect("Jita station");
    st + DVec3::new(0.0, 5.0e9 + k * 3.0e9, 0.0)
}

async fn anchor(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, name: &str) -> Result<PyRep, BridgeRpcError> {
    let ship = sess.ship_id.expect("ship");
    grant(ctx, OWNER, ship, type_id, InventoryFlag::Cargo, 1);
    rpc(ctx, sess, "structureDeployment", "Anchor", &[PyRep::Integer(i64::from(type_id)), PyRep::String(name.into())]).await
}

/// Triggers the lazy lifecycle sync through a real structure RPC and returns the record.
async fn observe(ctx: &ServerContext, sess: &mut SessionState, id: i64) -> Option<UpwellStructureRecord> {
    rpc(ctx, sess, "structureGuests", "GetGuests", &[PyRep::Integer(id)]).await.expect("GetGuests");
    ctx.db.with_snapshot(|snap| snap.structures.get(&id).cloned())
}

fn power(ctx: &ServerContext, id: i64) -> life::PowerState {
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        life::power_state(ctx, snap, &ext, snap.structures.get(&id).expect("structure"), now)
    })
}

/// Anchors `type_id`, runs anchoring, inserts its SDE quantum core and finishes onlining.
async fn online_structure(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, name: &str) -> i64 {
    let id = kv_i64(&anchor(ctx, sess, type_id, name).await.expect("Anchor"), "structureID");
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS);
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_NO_CORE);
    let core = attr(ctx, type_id, life::ATTR_REQUIRES_DEED_TYPE) as u32;
    // The pilot drags the core into the structure (core room) while docked.
    move_ship(ctx, sess, DVec3::from_array(ctx.db.with_snapshot(|s| s.structures[&id].position))).await;
    rpc(ctx, sess, "structureDocking", "Dock", &[PyRep::Integer(id)]).await.expect("Dock");
    let core_item = grant(ctx, OWNER, sess.ship_id.expect("ship"), core, InventoryFlag::Cargo, 1);
    rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(core_item), PyRep::Integer(id), PyRep::Integer(172)]).await.expect("move core into structure");
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_ONLINING);
    rpc(ctx, sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    ctx.advance_virtual_clock(life::ONLINING_SECS);
    let st = observe(ctx, sess, id).await.expect("st");
    assert_eq!(st.state, life::STATE_SHIELD_VULNERABLE);
    assert!(st.quantum_core_installed);
    id
}

/// Fits `module` into service slot 0 and marks it onlined by the owner (dogma onlining is
/// outside this module; the engine validates the onlining fuel requirement).
fn fit_online_service(ctx: &ServerContext, id: i64, module: u32) -> i64 {
    let item = grant(ctx, OWNER, id, module, InventoryFlag::ServiceSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| snap.module_online_states.insert(item, true));
    item
}

fn module_online(ctx: &ServerContext, item: i64) -> bool {
    ctx.db.with_snapshot(|snap| snap.module_online_states.get(&item).copied().unwrap_or(false))
}

/// Hands the structure to a rival corp at war with the pilot so it can be attacked in high-sec,
/// fits a weapon with ammo and sets the remaining HP of each layer.
fn prepare_siege(ctx: &ServerContext, id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let my_corp = snap.characters.get(&OWNER).map(|c| c.corporation_id).expect("corp");
        if let Some(s) = snap.structures.get_mut(&id) {
            s.owner_corp_id = RIVAL_CORP;
        }
        snap.wars.insert(77, eve_db::WarRecord { war_id: 77, aggressor_corp_id: my_corp, defender_corp_id: RIVAL_CORP, mutual: true, started_unix: 0, active: true });
        if let Some(w) = snap.items.get_mut(&100_000_005) {
            w.location_id = 100_000_001;
            w.flag = InventoryFlag::HiSlot0;
            w.singleton = true;
        }
    });
    grant(ctx, OWNER, 100_000_001, 185, InventoryFlag::HiSlot0, 10_000);
}

fn set_hp(ctx: &ServerContext, id: i64, shield: f64, armor: f64, hull: f64) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.structures.get_mut(&id) {
            s.shield_hp = shield;
            s.armor_hp = armor;
            s.hull_hp = hull;
        }
    });
}

async fn attack(ctx: &ServerContext, sess: &mut SessionState, id: i64) -> Result<PyRep, BridgeRpcError> {
    let (sys_id, ship_id) = (sess.solar_system_id.expect("sys"), sess.ship_id.expect("ship"));
    let sys = ctx.systems.get_or_spawn(sys_id);
    if !sys.get_target_locks(ship_id).await.unwrap_or_default().iter().any(|l| l.target_id == id && l.locked) {
        rpc(ctx, sess, "dogmaIM", "AddTarget", &[PyRep::Integer(id)]).await?;
        let _ = sys.step_manual(12.0).await;
    }
    let (pre_state, pre_rep) = ctx.db.with_snapshot(|s| {
        let ext = load_ext_state(s);
        (s.structures.get(&id).map(|st| st.state.clone()).unwrap_or_default(), ext.lifecycle.get(&id).and_then(|l| l.repair_until_unix))
    });
    let act = rpc(ctx, sess, "dogmaIM", "Activate", &[PyRep::Integer(100_000_005), PyRep::String("projectileFired".into()), PyRep::Integer(id)]).await?;
    let layer = match pre_state.as_str() { life::STATE_ARMOR_VULNERABLE => "armor", life::STATE_HULL_VULNERABLE | life::STATE_ANCHOR_VULNERABLE | life::STATE_ONLINING => "hull", _ => "shield" };
    Ok(ctx.db.with_snapshot(|s| {
        let st = s.structures.get(&id).expect("structure");
        let ext = load_ext_state(s);
        let core_tid = ctx.sde.types.get(&st.type_id).and_then(|t| t.default_attributes.get(&life::ATTR_REQUIRES_DEED_TYPE)).map(|v| *v as u32).unwrap_or(0);
        let wreck_id = s.items.values().find(|i| i.location_id == i64::from(sys_id) && s.item_positions.get(&i.item_id) == Some(&st.position) && (i.type_id == 26557 || ctx.sde.wrecks.values().any(|&w| w == i.type_id))).map(|i| i.item_id);
        let core_drop = s.items.values().filter(|i| i.type_id == core_tid && (i.location_id == i64::from(sys_id) || Some(i.location_id) == wreck_id)).map(|i| i.item_id).max().unwrap_or(0);
        let mut wraps: Vec<PyRep> = ext.asset_safety_wraps.values().filter(|w| w.source_structure_id == id).map(|w| PyRep::Integer(w.wrap_id)).collect();
        wraps.sort_by_key(|v| v.as_i64().unwrap_or(0));
        let paused = pre_rep.zip(ext.lifecycle.get(&id).and_then(|l| l.repair_until_unix)).is_some_and(|(a, b)| b > a);
        PyRep::keyval([("state", PyRep::String(st.state.clone())), ("reinforcedUntilUnix", PyRep::Integer(st.reinforced_until_unix as i64)), ("damagedLayer", PyRep::String(layer.into())), ("repairTimerPaused", PyRep::Bool(paused)), ("killmailID", PyRep::Integer(kv_i64(&act, "killID"))), ("droppedQuantumCoreItemID", PyRep::Integer(core_drop)), ("assetSafetyWrapIDs", PyRep::List(wraps))])
    }))
}

#[tokio::test]
async fn test_deploy_anchor_timers_restrictions_and_quantum_core() {
    // parity: feat:structures.deploy-anchor, feat:structures.quantum-core
    eve_testkit::attest(&["feat:structures.deploy-anchor", "feat:structures.quantum-core"]);
    let ctx = ServerContext::new();
    let clock = std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000));
    ctx.set_clock(clock);
    let mut sess = pilot_in_space(&ctx, 4001).await;

    // Distance rule: right next to the undock station the position is refused.
    let near = anchor(&ctx, &mut sess, 35832, "Too Close").await.expect_err("within 1,000 km of a station");
    assert_eq!(near.code, "TooCloseToStructureOrStation");
    // Sec-band rule from the SDE: Keepstar has disallowInHighSec.
    assert!(attr(&ctx, 35834, life::ATTR_DISALLOW_IN_HIGHSEC) > 0.0);
    move_ship(&ctx, &sess, open_space(&ctx, 0.0)).await;
    let ks = anchor(&ctx, &mut sess, 35834, "Highsec Keepstar").await.expect_err("Keepstar in high-sec");
    assert_eq!(ks.code, "CannotAnchorInHighSec");

    // Deploy: structure item consumed, core NOT consumed, hull-only 15-minute anchoring timer.
    let ship = sess.ship_id.expect("ship");
    let core_type = attr(&ctx, 35832, life::ATTR_REQUIRES_DEED_TYPE) as u32;
    let spare_core = grant(&ctx, OWNER, ship, core_type, InventoryFlag::Cargo, 1);
    let t0 = ctx.now_unix();
    let hulls_before = ctx.db.with_snapshot(|snap| snap.items.values().filter(|i| i.location_id == ship && i.type_id == 35832).map(|i| i.quantity).sum::<i32>());
    let res = anchor(&ctx, &mut sess, 35832, "Frontier Astrahus").await.expect("Anchor");
    let id = kv_i64(&res, "structureID");
    assert_eq!(kv_str(&res, "state"), life::STATE_ANCHOR_VULNERABLE);
    assert_eq!(kv_i64(&res, "stateTimerEndUnix"), (t0 + life::ANCHOR_VULNERABLE_SECS) as i64);
    assert_eq!(kv_i64(&res, "quantumCoreTypeID"), i64::from(core_type));
    let st = observe(&ctx, &mut sess, id).await.expect("structure");
    assert_eq!((st.shield_hp, st.armor_hp, st.hull_hp), (0.0, 0.0, attr(&ctx, 35832, life::ATTR_HP)));
    assert!(!st.quantum_core_installed);
    ctx.db.with_snapshot(|snap| {
        assert!(snap.items.contains_key(&spare_core), "core is not consumed by Anchor");
        let hulls_after: i32 = snap.items.values().filter(|i| i.location_id == ship && i.type_id == 35832).map(|i| i.quantity).sum();
        assert_eq!(hulls_after, hulls_before, "exactly the granted structure item was consumed");
    });
    // A second structure at the same spot violates the 1,000 km rule.
    let second = anchor(&ctx, &mut sess, 35832, "Overlap").await.expect_err("too close to structure");
    assert_eq!(second.code, "TooCloseToStructureOrStation");

    // 15 min -> 24 h invulnerable anchoring -> NoCore.
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS - 1);
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().state, life::STATE_ANCHOR_VULNERABLE);
    ctx.advance_virtual_clock(1);
    let st = observe(&ctx, &mut sess, id).await.unwrap();
    assert_eq!(st.state, life::STATE_ANCHORING);
    assert_eq!(st.reinforced_until_unix, t0 + life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS);
    ctx.advance_virtual_clock(life::ANCHORING_SECS);
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().state, life::STATE_NO_CORE);

    // Without a core the structure never onlines.
    ctx.advance_virtual_clock(3 * 86_400);
    let st = observe(&ctx, &mut sess, id).await.unwrap();
    assert_eq!(st.state, life::STATE_NO_CORE);
    assert_eq!((st.shield_hp, st.armor_hp), (0.0, 0.0));

    // A core of the wrong type does nothing; the SDE deed type starts the 15-minute onlining.
    move_ship(&ctx, &sess, DVec3::from_array(ctx.db.with_snapshot(|s| s.structures[&id].position))).await;
    rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(id)]).await.expect("Dock");
    let wrong_core = attr(&ctx, 35834, life::ATTR_REQUIRES_DEED_TYPE) as u32;
    let wrong = grant(&ctx, OWNER, ship, wrong_core, InventoryFlag::Cargo, 1);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(wrong), PyRep::Integer(id), PyRep::Integer(4)]).await.expect("move wrong core");
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().state, life::STATE_NO_CORE);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(spare_core), PyRep::Integer(id), PyRep::Integer(172)]).await.expect("move core");
    let t_core = ctx.now_unix();
    let st = observe(&ctx, &mut sess, id).await.unwrap();
    assert_eq!(st.state, life::STATE_ONLINING);
    assert!(st.quantum_core_installed);
    assert_eq!(st.reinforced_until_unix, t_core + life::ONLINING_SECS);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&spare_core), "core consumed into the core room");
        assert!(snap.items.contains_key(&wrong), "wrong core untouched");
    });
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    ctx.advance_virtual_clock(life::ONLINING_SECS);
    let st = observe(&ctx, &mut sess, id).await.unwrap();
    assert_eq!(st.state, life::STATE_SHIELD_VULNERABLE);
    // Online without services = low power: SDE shield, armor and hull.
    assert_eq!((st.shield_hp, st.armor_hp, st.hull_hp), (attr(&ctx, 35832, life::ATTR_SHIELD_CAPACITY), attr(&ctx, 35832, life::ATTR_ARMOR_HP), attr(&ctx, 35832, life::ATTR_HP)));
    assert_eq!(power(&ctx, id), life::PowerState::LowPower);

    // Core drops on destruction (SDE deed type, in space at the structure position).
    prepare_siege(&ctx, id);
    set_hp(&ctx, id, 0.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&id).unwrap().state = life::STATE_HULL_VULNERABLE.into());
    set_hp(&ctx, id, 0.0, 0.0, 1.0);
    let kill = attack(&ctx, &mut sess, id).await.expect("final blow");
    assert_eq!(kv_str(&kill, "state"), life::STATE_DESTROYED);
    let core_drop = kv_i64(&kill, "droppedQuantumCoreItemID");
    let wreck_id = ctx.db.with_snapshot(|snap| {
        snap.items.values().find(|i| i.location_id == i64::from(JITA) && snap.item_positions.get(&i.item_id) == Some(&st.position) && (i.type_id == 26557 || ctx.sde.wrecks.values().any(|&w| w == i.type_id))).map(|i| i.item_id).expect("wreck")
    });
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&core_drop).expect("dropped core");
        assert_eq!(it.type_id, core_type);
        assert_eq!(it.location_id, wreck_id);
    });
}

#[tokio::test]
async fn test_fuel_consumption_low_power_and_abandoned() {
    // parity: feat:structures.fuel-consumption, feat:structures.abandoned
    // parity: feat:lifecycle.structure-abandoned_destroyed
    eve_testkit::attest(&[
        "feat:structures.fuel-consumption",
        "feat:structures.abandoned",
        "feat:lifecycle.structure-abandoned_destroyed",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = pilot_in_space(&ctx, 4002).await;
    move_ship(&ctx, &sess, open_space(&ctx, 1.0)).await;
    let id = online_structure(&ctx, &mut sess, 35832, "Fuel Astrahus").await;
    let clone_bay = 35894u32; // Standup Cloning Center I (fits Astrahus via canFitShipGroup)
    let per_hour = attr(&ctx, clone_bay, life::ATTR_SERVICE_FUEL_PER_HOUR) as u32;
    let online_need = attr(&ctx, clone_bay, life::ATTR_SERVICE_FUEL_ONLINE) as u32;
    let fuel_group = attr(&ctx, clone_bay, life::ATTR_SERVICE_FUEL_GROUP) as u32;
    let fuel_type = ctx.sde.types.values().filter(|t| t.group_id == fuel_group && t.published).map(|t| t.type_id).min().expect("fuel block type");

    // Onlining is refused while the fuel bay lacks the onlining amount.
    let svc = fit_online_service(&ctx, id, clone_bay);
    observe(&ctx, &mut sess, id).await;
    assert!(!module_online(&ctx, svc));
    assert_eq!(power(&ctx, id), life::PowerState::LowPower);

    // Only SDE fuel-group items are accepted into SpecializedFuelBay (flag 133).
    let ship = sess.ship_id.expect("ship");
    let trit_item = grant(&ctx, OWNER, ship, 34, InventoryFlag::Cargo, 5_000);
    let bad = rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(trit_item), PyRep::Integer(id), PyRep::Integer(133), PyRep::Integer(1_000)]).await.expect_err("tritanium is not fuel");
    assert_eq!(bad.code, "InvalidInventoryFlag");
    let factory = 35878u32;
    let svc_online = (f64::from(online_need) * 0.75).round() as u32;
    let factory_online = attr(&ctx, factory, life::ATTR_SERVICE_FUEL_ONLINE) as u32;
    let load = svc_online + factory_online + 30 * per_hour;
    let fuel_item = grant(&ctx, OWNER, ship, fuel_type, InventoryFlag::Cargo, load as i32);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(fuel_item), PyRep::Integer(id), PyRep::Integer(133), PyRep::Integer(i64::from(load))]).await.expect("fuel bay");
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().fuel_blocks, load);

    // Now the module comes online -> deducts startup fuel, enters full power, shield multiplied per SDE full-power attributes.
    ctx.db.with_mut_snapshot(|snap| snap.module_online_states.insert(svc, true));
    let st = observe(&ctx, &mut sess, id).await.unwrap();
    assert_eq!(st.fuel_blocks, load - svc_online);
    assert!(module_online(&ctx, svc));
    assert_eq!(power(&ctx, id), life::PowerState::FullPower);
    let base_shield = attr(&ctx, 35832, life::ATTR_SHIELD_CAPACITY);
    let full_shield = base_shield * attr(&ctx, 35832, life::ATTR_STRUCTURE_FULL_POWER_HP_MULT) * attr(&ctx, clone_bay, life::ATTR_SERVICE_FULL_POWER_HP_MULT);
    assert!(st.shield_hp <= base_shield + 1e-6);

    // Hourly burn per online service module (applying structure fuel bonus).
    let burn_h = (f64::from(per_hour) * 0.75).round() as u32;
    ctx.advance_virtual_clock(3 * 3_600);
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().fuel_blocks, load - svc_online - 3 * burn_h);
    // A second online module deducts its startup fuel and adds its own hourly amount (Standup Manufacturing Plant I).
    let factory_hour = attr(&ctx, factory, life::ATTR_SERVICE_FUEL_PER_HOUR) as u32;
    let factory_item = grant(&ctx, OWNER, id, factory, InventoryFlag::ServiceSlot1, 1);
    ctx.db.with_mut_snapshot(|snap| snap.module_online_states.insert(factory_item, true));
    observe(&ctx, &mut sess, id).await;
    assert!(module_online(&ctx, factory_item));
    // Standup Manufacturing Plant I belongs to Engineering Complexes, not Citadels, so Astrahus role bonus does not apply.
    let both_burn = burn_h + factory_hour;
    ctx.advance_virtual_clock(3_600);
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().fuel_blocks, load - svc_online - factory_online - 3 * burn_h - both_burn);
    ctx.db.with_mut_snapshot(|snap| snap.module_online_states.insert(factory_item, false));
    let cur_blocks = load - svc_online - factory_online - 3 * burn_h - both_burn;
    ctx.advance_virtual_clock(1_800);
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().fuel_blocks, cur_blocks, "no partial-hour burn");
    // A repair to full happens at full-power shield (damage it a little and let it repair).
    set_hp(&ctx, id, 1.0, attr(&ctx, 35832, life::ATTR_ARMOR_HP), attr(&ctx, 35832, life::ATTR_HP));
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(id).or_default().repair_until_unix = Some(ctx.now_unix());
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });
    assert!((observe(&ctx, &mut sess, id).await.unwrap().shield_hp - full_shield).abs() < 1e-6);

    // Run dry: when an hour cannot be paid the module goes offline -> low power, shield clamped.
    let remaining_hours = u64::from(cur_blocks / burn_h);
    ctx.advance_virtual_clock((remaining_hours + 1) * 3_600);
    let st = observe(&ctx, &mut sess, id).await.unwrap();
    assert!(!module_online(&ctx, svc), "services offline when fuel runs out");
    assert_eq!(power(&ctx, id), life::PowerState::LowPower);
    assert!(st.shield_hp <= base_shield + 1e-6, "low power shield is the weaker SDE base");
    assert!(st.fuel_blocks < burn_h);

    // Seven days of low power -> abandoned.
    ctx.advance_virtual_clock(life::ABANDON_AFTER_LOW_POWER_SECS - 3 * 3_600);
    observe(&ctx, &mut sess, id).await;
    assert_eq!(power(&ctx, id), life::PowerState::LowPower);
    ctx.advance_virtual_clock(3 * 3_600);
    observe(&ctx, &mut sess, id).await;
    assert_eq!(power(&ctx, id), life::PowerState::Abandoned);

    // Abandoned = single timer: no armor or hull reinforcement, destroyed in one fight.
    // feat:lifecycle.structure-abandoned_destroyed: hangar contents drop as loot instead of asset safety
    let abandoned_cargo = grant(&ctx, OWNER, id, 34, InventoryFlag::Hangar, 250);
    prepare_siege(&ctx, id);
    set_hp(&ctx, id, 1.0, 1.0, 1.0);
    let a = attack(&ctx, &mut sess, id).await.expect("shield");
    assert_eq!(kv_str(&a, "state"), life::STATE_ARMOR_VULNERABLE);
    let b = attack(&ctx, &mut sess, id).await.expect("armor");
    assert_eq!(kv_str(&b, "state"), life::STATE_HULL_VULNERABLE);
    let c = attack(&ctx, &mut sess, id).await.expect("hull");
    assert_eq!(kv_str(&c, "state"), life::STATE_DESTROYED);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&abandoned_cargo).map(|i| i.location_id), Some(i64::from(JITA)), "abandoned structure drops cargo to space");
    });
}

#[tokio::test]
async fn test_low_power_large_structure_skips_armor_reinforcement() {
    // parity: feat:structures.low-power
    eve_testkit::attest(&["feat:structures.low-power"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = pilot_in_space(&ctx, 4003).await;
    move_ship(&ctx, &sess, open_space(&ctx, 2.0)).await;
    // Full-power Fortizar: shield loss -> armor reinforcement.
    let full = online_structure(&ctx, &mut sess, 35833, "Powered Fortizar").await;
    let ship = sess.ship_id.expect("ship");
    let fuel_group = attr(&ctx, 35894, life::ATTR_SERVICE_FUEL_GROUP) as u32;
    let fuel_type = ctx.sde.types.values().filter(|t| t.group_id == fuel_group && t.published).map(|t| t.type_id).min().unwrap();
    let fuel_item = grant(&ctx, OWNER, ship, fuel_type, InventoryFlag::Cargo, 5_000);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(fuel_item), PyRep::Integer(full), PyRep::Integer(133)]).await.expect("fuel bay");
    let svc = fit_online_service(&ctx, full, 35894);
    observe(&ctx, &mut sess, full).await;
    assert!(module_online(&ctx, svc));
    assert_eq!(power(&ctx, full), life::PowerState::FullPower);
    // Second Fortizar 2,000 km away, fueled for only a few hours of service.
    move_ship(&ctx, &sess, open_space(&ctx, 2.0) + DVec3::new(2.0e6, 0.0, 0.0)).await;
    let low = online_structure(&ctx, &mut sess, 35833, "Dark Fortizar").await;
    let _per_hour = attr(&ctx, 35894, life::ATTR_SERVICE_FUEL_PER_HOUR) as u32;
    let online_need = attr(&ctx, 35894, life::ATTR_SERVICE_FUEL_ONLINE) as u32;
    let low_fuel = grant(&ctx, OWNER, ship, fuel_type, InventoryFlag::Cargo, online_need as i32);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(low_fuel), PyRep::Integer(low), PyRep::Integer(133)]).await.expect("fuel low");
    let low_svc = fit_online_service(&ctx, low, 35894);
    observe(&ctx, &mut sess, low).await;
    assert!(module_online(&ctx, low_svc));
    assert_eq!(power(&ctx, low), life::PowerState::FullPower);
    let base_shield = attr(&ctx, 35833, life::ATTR_SHIELD_CAPACITY);
    let full_shield = base_shield * attr(&ctx, 35833, life::ATTR_STRUCTURE_FULL_POWER_HP_MULT) * attr(&ctx, 35894, life::ATTR_SERVICE_FULL_POWER_HP_MULT);
    assert!(full_shield > base_shield);
    set_hp(&ctx, low, full_shield, attr(&ctx, 35833, life::ATTR_ARMOR_HP), attr(&ctx, 35833, life::ATTR_HP));
    // Out of fuel: the service goes offline, the shield drops to the weak low-power maximum.
    ctx.advance_virtual_clock((u64::from(online_need / 8) + 2) * 3_600);
    let st = observe(&ctx, &mut sess, low).await.unwrap();
    assert!(!module_online(&ctx, low_svc), "services offline");
    assert_eq!(power(&ctx, low), life::PowerState::LowPower);
    assert_eq!(st.shield_hp, base_shield, "shield weak in low power");

    prepare_siege(&ctx, low);
    set_hp(&ctx, low, 1.0, attr(&ctx, 35833, life::ATTR_ARMOR_HP), attr(&ctx, 35833, life::ATTR_HP));
    let res = attack(&ctx, &mut sess, low).await.expect("strip low-power shield");
    assert_eq!(kv_str(&res, "state"), life::STATE_ARMOR_VULNERABLE, "low-power large structures skip armor reinforcement");

    move_ship(&ctx, &sess, open_space(&ctx, 2.0)).await;
    prepare_siege(&ctx, full);
    set_hp(&ctx, full, 1.0, attr(&ctx, 35833, life::ATTR_ARMOR_HP), attr(&ctx, 35833, life::ATTR_HP));
    let res = attack(&ctx, &mut sess, full).await.expect("strip full-power shield");
    assert_eq!(kv_str(&res, "state"), life::STATE_ARMOR_REINFORCED);
}

#[tokio::test]
async fn test_reinforcement_cycle_per_size_and_security_band() {
    // parity: feat:structures.reinforce-cycle
    eve_testkit::attest(&["feat:structures.reinforce-cycle"]);
    use life::{HullClass::*, PowerState::*, SpaceClass::*};
    // Wiki tables (DERIVED constants) for every size and space band.
    assert_eq!(life::armor_reinforcement_secs(Medium, HighSec, FullPower), Some(108 * 3_600));
    assert_eq!(life::armor_reinforcement_secs(Medium, LowNullSec, LowPower), Some(84 * 3_600));
    assert_eq!(life::armor_reinforcement_secs(Medium, Wormhole, FullPower), Some(60 * 3_600));
    assert_eq!(life::armor_reinforcement_secs(Large, HighSec, FullPower), Some(24 * 3_600));
    assert_eq!(life::armor_reinforcement_secs(XLarge, Wormhole, FullPower), Some(24 * 3_600));
    assert_eq!(life::hull_reinforcement_secs(Medium, LowNullSec, FullPower), None);
    assert_eq!(life::hull_reinforcement_secs(Large, HighSec, FullPower), Some(108 * 3_600));
    assert_eq!(life::hull_reinforcement_secs(XLarge, LowNullSec, FullPower), Some(60 * 3_600));
    assert_eq!(life::hull_reinforcement_secs(Large, Wormhole, LowPower), Some(36 * 3_600));
    assert_eq!(life::next_reinforce_exit(86_400 + 5 * 3_600, 3), 2 * 86_400 + 3 * 3_600);
    assert_eq!(life::next_reinforce_exit(86_400 + 5 * 3_600, 19), 86_400 + 19 * 3_600);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = pilot_in_space(&ctx, 4004).await;
    move_ship(&ctx, &sess, open_space(&ctx, 3.0)).await;

    // Medium (Astrahus) in high-sec: shield -> armor reinforced (4.5 d to the reinforce hour),
    // invulnerable meanwhile, armor -> hull vulnerable (no hull timer), hull -> destroyed.
    let med = online_structure(&ctx, &mut sess, 35832, "Medium Target").await;
    let hour = ctx.db.with_snapshot(|snap| snap.structure_vulnerability_windows.get(&med).map(|w| w.0).expect("hour"));
    prepare_siege(&ctx, med);
    set_hp(&ctx, med, 1.0, attr(&ctx, 35832, life::ATTR_ARMOR_HP), attr(&ctx, 35832, life::ATTR_HP));
    let now = ctx.now_unix();
    let r = attack(&ctx, &mut sess, med).await.expect("shield down");
    assert_eq!(kv_str(&r, "state"), life::STATE_ARMOR_REINFORCED);
    let exit = life::next_reinforce_exit(now + 108 * 3_600, hour);
    let actual_exit = kv_i64(&r, "reinforcedUntilUnix") as u64;
    assert!((actual_exit as i64 - exit as i64).unsigned_abs() <= 2 * 3_600);
    assert_eq!(exit % 86_400 / 3_600, hour);
    let inv = attack(&ctx, &mut sess, med).await.expect_err("reinforced");
    assert_eq!(inv.code, "StructureReinforcedInvulnerable");
    ctx.advance_virtual_clock(actual_exit - ctx.now_unix());
    assert_eq!(observe(&ctx, &mut sess, med).await.unwrap().state, life::STATE_ARMOR_VULNERABLE);
    set_hp(&ctx, med, 0.0, 1.0, attr(&ctx, 35832, life::ATTR_HP));
    let r = attack(&ctx, &mut sess, med).await.expect("armor down");
    assert_eq!(kv_str(&r, "state"), life::STATE_HULL_VULNERABLE, "medium structures skip hull reinforcement");
    set_hp(&ctx, med, 0.0, 0.0, 1.0);
    let r = attack(&ctx, &mut sess, med).await.expect("hull down");
    assert_eq!(kv_str(&r, "state"), life::STATE_DESTROYED);

    // Large (Fortizar, full power) in high-sec: 24 h armor timer, then 4.5 d hull timer.
    move_ship(&ctx, &sess, open_space(&ctx, 3.0) + DVec3::new(3.0e6, 0.0, 0.0)).await;
    let large = online_structure(&ctx, &mut sess, 35833, "Large Target").await;
    let fuel_group = attr(&ctx, 35894, life::ATTR_SERVICE_FUEL_GROUP) as u32;
    let fuel_type = ctx.sde.types.values().filter(|t| t.group_id == fuel_group && t.published).map(|t| t.type_id).min().unwrap();
    let large_fuel = grant(&ctx, OWNER, sess.ship_id.unwrap(), fuel_type, InventoryFlag::Cargo, 20_000);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(large_fuel), PyRep::Integer(large), PyRep::Integer(133)]).await.expect("fuel");
    let svc = fit_online_service(&ctx, large, 35894);
    observe(&ctx, &mut sess, large).await;
    assert!(module_online(&ctx, svc));
    let lhour = ctx.db.with_snapshot(|snap| snap.structure_vulnerability_windows.get(&large).map(|w| w.0).unwrap());
    prepare_siege(&ctx, large);
    set_hp(&ctx, large, 1.0, attr(&ctx, 35833, life::ATTR_ARMOR_HP), attr(&ctx, 35833, life::ATTR_HP));
    let fight_start = ctx.now_unix();
    let r = attack(&ctx, &mut sess, large).await.expect("large shield");
    let lex = life::next_reinforce_exit(fight_start + 24 * 3_600, lhour);
    let lactual = kv_i64(&r, "reinforcedUntilUnix") as u64;
    assert!((lactual as i64 - lex as i64).unsigned_abs() <= 3 * 3_600);
    ctx.advance_virtual_clock(lactual - ctx.now_unix());
    assert_eq!(observe(&ctx, &mut sess, large).await.unwrap().state, life::STATE_ARMOR_VULNERABLE);
    set_hp(&ctx, large, 0.0, 1.0, attr(&ctx, 35833, life::ATTR_HP));
    let r = attack(&ctx, &mut sess, large).await.expect("large armor");
    assert_eq!(kv_str(&r, "state"), life::STATE_HULL_REINFORCED);
    let lex = life::next_reinforce_exit(fight_start + (24 + 108) * 3_600, lhour);
    let lactual = kv_i64(&r, "reinforcedUntilUnix") as u64;
    assert!((lactual as i64 - lex as i64).unsigned_abs() <= 3 * 3_600);
}

#[tokio::test]
async fn test_damage_cap_and_repair_timer_with_pause_threshold() {
    // parity: feat:structures.damage-cap, feat:structures.repair-timer
    eve_testkit::attest(&["feat:structures.damage-cap", "feat:structures.repair-timer"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = pilot_in_space(&ctx, 4005).await;
    move_ship(&ctx, &sess, open_space(&ctx, 4.0)).await;
    let id = online_structure(&ctx, &mut sess, 35832, "Cap Astrahus").await;
    let now = ctx.now_unix();
    // Damage cap per layer and per structure type comes from SDE *DamageLimit attributes.
    let armor_cap = attr(&ctx, 35832, life::ATTR_ARMOR_DAMAGE_LIMIT);
    let ks_armor_cap = attr(&ctx, 35834, life::ATTR_ARMOR_DAMAGE_LIMIT);
    assert!(ks_armor_cap > armor_cap);
    let out = ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&id).unwrap().state = life::STATE_ARMOR_VULNERABLE.into();
        let mut ext = load_ext_state(snap);
        life::apply_structure_damage(&ctx, snap, &mut ext, id, 1.0e9, 5.0, OWNER, now).expect("damage")
    });
    assert_eq!(out.layer, "armor");
    assert_eq!(out.applied_damage, armor_cap * 5.0, "1e9 raw damage over 5 s is capped at the SDE armor limit");
    let hull_cap = attr(&ctx, 35832, life::ATTR_STRUCTURE_DAMAGE_LIMIT);
    let out = ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&id).unwrap().state = life::STATE_HULL_VULNERABLE.into();
        let mut ext = load_ext_state(snap);
        life::apply_structure_damage(&ctx, snap, &mut ext, id, 10.0, 5.0, OWNER, now).expect("damage")
    });
    assert_eq!(out.applied_damage, 10.0, "damage under the cap is applied unchanged");
    assert!(hull_cap * 5.0 > 10.0);
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&id).unwrap().state = life::STATE_SHIELD_VULNERABLE.into());

    // Repair timer: a light hit (below the SDE pause threshold) starts the 15-minute timer, which
    // completes on time and restores full HP.
    prepare_siege(&ctx, id);
    let full_shield = attr(&ctx, 35832, life::ATTR_SHIELD_CAPACITY);
    set_hp(&ctx, id, full_shield, attr(&ctx, 35832, life::ATTR_ARMOR_HP), attr(&ctx, 35832, life::ATTR_HP));
    let hit_at = ctx.now_unix();
    let r = attack(&ctx, &mut sess, id).await.expect("light hit");
    assert_eq!(kv_str(&r, "damagedLayer"), "shield");
    assert!(!matches!(r, PyRep::KeyVal(ref e) if e.get("repairTimerPaused") == Some(&PyRep::Bool(true))));
    let damaged = observe(&ctx, &mut sess, id).await.unwrap().shield_hp;
    assert!(damaged < full_shield);
    ctx.advance_virtual_clock(life::REPAIR_SHIELD_ARMOR_SECS - 60);
    assert!(observe(&ctx, &mut sess, id).await.unwrap().shield_hp < full_shield, "still repairing");
    ctx.advance_virtual_clock(60 + (hit_at + life::REPAIR_SHIELD_ARMOR_SECS).saturating_sub(ctx.now_unix()));
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().shield_hp, full_shield);

    // Heavy fire at/above the SDE pause threshold freezes the timer for that cycle.
    let pause = attr(&ctx, 35832, life::ATTR_PAUSE_SHIELD_REPAIR_DPS);
    let t = ctx.now_unix();
    let paused = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let a = life::apply_structure_damage(&ctx, snap, &mut ext, id, pause * 10.0, 10.0, OWNER, t).expect("first");
        let b = life::apply_structure_damage(&ctx, snap, &mut ext, id, pause * 10.0, 10.0, OWNER, t + 10).expect("heavy");
        let c = life::apply_structure_damage(&ctx, snap, &mut ext, id, pause * 1.0, 10.0, OWNER, t + 20).expect("light");
        (a.repair_paused, b.repair_paused, c.repair_paused, ext.lifecycle.get(&id).and_then(|l| l.repair_until_unix))
    });
    assert_eq!((paused.0, paused.1, paused.2), (true, true, false));
    assert_eq!(paused.3, Some(t + 10 + 10 + life::REPAIR_SHIELD_ARMOR_SECS), "two 10 s qualifying cycles push the 15-minute timer");
}

#[tokio::test]
async fn test_killmail_core_fitting_drop_and_automatic_asset_safety() {
    // parity: feat:structures.structure-killmail, feat:structures.asset-safety-auto
    eve_testkit::attest(&["feat:structures.structure-killmail", "feat:structures.asset-safety-auto"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = pilot_in_space(&ctx, 4006).await;
    move_ship(&ctx, &sess, open_space(&ctx, 5.0)).await;
    let id = online_structure(&ctx, &mut sess, 35832, "Doomed Astrahus").await;
    let cargo = grant(&ctx, OWNER, id, 34, InventoryFlag::Hangar, 1_000);
    let rig = grant(&ctx, OWNER, id, 37_146, InventoryFlag::RigSlot0, 1);
    let mod_a = grant(&ctx, OWNER, id, 35_923, InventoryFlag::HiSlot0, 1);
    let mod_b = grant(&ctx, OWNER, id, 35_923, InventoryFlag::HiSlot1, 1);
    let pos = observe(&ctx, &mut sess, id).await.unwrap().position;
    prepare_siege(&ctx, id);
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&id).unwrap().state = life::STATE_HULL_VULNERABLE.into());
    set_hp(&ctx, id, 0.0, 0.0, 1.0);
    let r = attack(&ctx, &mut sess, id).await.expect("kill");
    assert_eq!(kv_str(&r, "state"), life::STATE_DESTROYED);
    let core_drop = kv_i64(&r, "droppedQuantumCoreItemID");
    let core_type = attr(&ctx, 35832, life::ATTR_REQUIRES_DEED_TYPE) as u32;
    let wreck_id = ctx.db.with_snapshot(|snap| {
        snap.items.values().find(|i| i.location_id == i64::from(JITA) && snap.item_positions.get(&i.item_id) == Some(&pos) && (i.type_id == 26557 || ctx.sde.wrecks.values().any(|&w| w == i.type_id))).map(|i| i.item_id).expect("wreck")
    });
    ctx.db.with_snapshot(|snap| assert_eq!(snap.items.get(&core_drop).map(|i| (i.type_id, i.location_id)), Some((core_type, wreck_id))));
    let km = kv_i64(&r, "killmailID");
    let wraps: Vec<i64> = match &r {
        PyRep::KeyVal(e) => match e.get("assetSafetyWrapIDs") {
            Some(PyRep::List(l)) => l.iter().filter_map(|v| v.as_i64()).collect(),
            _ => vec![],
        },
        _ => vec![],
    };
    let expected_station = life::nearest_npc_station_from_pos(&ctx, JITA, glam::DVec3::from_array(pos));
    ctx.db.with_snapshot(|snap| {
        let k = snap.killmails.get(&km).expect("killmail");
        assert_eq!((k.ship_type_id, k.victim_character_id, k.attacker_character_id, k.solar_system_id), (35832, RIVAL_CORP, OWNER, JITA));
        assert_eq!(k.total_value_isk, ctx.sde.types.get(&35832).unwrap().base_price);
        assert!(!snap.items.contains_key(&rig), "rigs are destroyed");
        let a = snap.items.get(&mod_a).expect("first module drops");
        assert_eq!((a.location_id, a.flag), (i64::from(JITA), InventoryFlag::None));
        assert_eq!(snap.item_positions.get(&mod_a), Some(&pos));
        assert!(!snap.items.contains_key(&mod_b), "second module destroyed (DERIVED alternation)");
        let ext = load_ext_state(snap);
        assert_eq!(wraps.len(), 1);
        let w = ext.asset_safety_wraps.get(&wraps[0]).expect("wrap");
        assert_eq!((w.owner_id, w.item_ids.clone(), w.nearest_npc_station_id), (OWNER, vec![cargo], expected_station));
        assert_eq!(snap.items.get(&cargo).map(|i| (i.location_id, i.flag)), Some((wraps[0], InventoryFlag::Locked)));
    });

    // Wormhole structures drop their contents as loot instead of using asset safety.
    let wh_sys = ctx.sde.solar_systems.keys().copied().filter(|s| *s >= 31_000_000 && *s < 32_000_000).min().expect("J-space system");
    let wh_id = 1_000_000_900_001i64;
    let wh_item = grant(&ctx, OWNER, wh_id, 34, InventoryFlag::Hangar, 500);
    let out = ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(wh_id, UpwellStructureRecord { structure_id: wh_id, type_id: 35832, owner_corp_id: RIVAL_CORP, solar_system_id: wh_sys, name: "J-space Astrahus".into(), position: [1.0, 2.0, 3.0], state: life::STATE_HULL_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 0, shield_hp: 0.0, armor_hp: 0.0, hull_hp: 0.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None });
        let mut ext = load_ext_state(snap);
        life::destroy_structure(&ctx, snap, &mut ext, wh_id, OWNER, ctx.now_unix())
    });
    assert!(out.asset_safety_wrap_ids.is_empty());
    assert_eq!(out.dropped_item_ids, vec![wh_item]);
    assert!(out.killmail_id.is_some() && out.dropped_core_item_id.is_some());
    ctx.db.with_snapshot(|snap| assert_eq!(snap.items.get(&wh_item).map(|i| i.location_id), Some(i64::from(wh_sys))));
}

#[tokio::test]
async fn test_unanchor_cancel_and_packaging_after_seven_days() {
    // parity: feat:structures.unanchor
    // parity: feat:lifecycle.structure-leave_space
    eve_testkit::attest(&["feat:structures.unanchor", "feat:lifecycle.structure-leave_space"]);
    let ctx = ServerContext::new();
    // Deterministic virtual time: the 7-day timer assertion must not race the host wall clock.
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let mut sess = pilot_in_space(&ctx, 4007).await;
    move_ship(&ctx, &sess, open_space(&ctx, 6.0)).await;
    let id = online_structure(&ctx, &mut sess, 35832, "Retiring Astrahus").await;
    let pos = observe(&ctx, &mut sess, id).await.unwrap().position;
    let cargo = grant(&ctx, OWNER, id, 35, InventoryFlag::Hangar, 100);
    let rig = grant(&ctx, OWNER, id, 37_146, InventoryFlag::RigSlot0, 1);

    // Unanchor starts a 7-day timer; cancelling aborts it (structure survives past 7 days).
    rpc(&ctx, &mut sess, "structureDeployment", "Unanchor", &[PyRep::Integer(id)]).await.expect("Unanchor");
    ctx.advance_virtual_clock(86_400);
    rpc(&ctx, &mut sess, "structureDeployment", "CancelUnanchor", &[PyRep::Integer(id)]).await.expect("CancelUnanchor");
    ctx.advance_virtual_clock(8 * 86_400);
    assert!(observe(&ctx, &mut sess, id).await.is_some(), "cancelled unanchor keeps the structure");

    // Restart and let it complete: the structure is packaged in space with its core dropped.
    let started = ctx.now_unix();
    rpc(&ctx, &mut sess, "structureDeployment", "Unanchor", &[PyRep::Integer(id)]).await.expect("Unanchor again");
    ctx.advance_virtual_clock(7 * 86_400 - 60);
    assert!(observe(&ctx, &mut sess, id).await.is_some(), "still unanchoring");
    ctx.advance_virtual_clock(60);
    assert!(observe(&ctx, &mut sess, id).await.is_none(), "unanchored structure leaves space");
    assert_eq!(ctx.now_unix(), started + 7 * 86_400);
    let core_type = attr(&ctx, 35832, life::ATTR_REQUIRES_DEED_TYPE) as u32;
    ctx.db.with_snapshot(|snap| {
        let in_space: Vec<&InventoryItemRecord> = snap.items.values().filter(|i| i.location_id == i64::from(JITA) && snap.item_positions.get(&i.item_id) == Some(&pos)).collect();
        assert!(in_space.iter().any(|i| i.type_id == 35832 && i.quantity == 1), "packaged structure item");
        assert!(in_space.iter().any(|i| i.type_id == core_type), "quantum core drops on unanchor");
        assert!(!snap.items.contains_key(&rig), "rigs are lost");
        let ext = load_ext_state(snap);
        let wrap = ext.asset_safety_wraps.values().find(|w| w.source_structure_id == id).expect("hangar contents in asset safety");
        assert_eq!(wrap.item_ids, vec![cargo]);
    });
}

#[allow(dead_code, unused_imports)]
pub(crate) mod r4_mining_regressions_ext {
    use super::*;

//  Round-4 Track 2 Regression Tests Part 2 (#170 through #175):
//  Upwell Structure Control, ESS Reserve Unlock, Incursion Wave Respawn, FW Rearguard LP, Mutaplasmids & POCO Tax.


use crate::common::login_pilot;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[tokio::test]
async fn test_r4_170_structure_release_control_restores_docked_ship() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(17001);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let corp_id = 98_170_001;
    let struct_id = 950_170_001i64;
    let ship_id = 100_000_001i64;
    let kw = HashMap::new();

    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .unwrap();
    sess.station_id = None;
    sess.structure_id = Some(struct_id);
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
            ch.active_ship_id = ship_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = struct_id;
            sh.flag = InventoryFlag::Hangar;
        }
        let mut roles = HashMap::new();
        roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Gunner 170 Corp".into(),
                ticker: "G170".into(),
                ceo_id: char_id,
                members: HashSet::from([char_id]),
                member_roles: roles,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "170 Astrahus".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 10_000_000.0,
                armor_hp: 10_000_000.0,
                hull_hp: 10_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    ctx.dispatch_rpc(
        &mut sess,
        "structureControl",
        None,
        "TakeControl",
        &[PyRep::Integer(struct_id)],
        &kw,
        None,
    )
    .await
    .expect("TakeControl");
    assert_eq!(sess.ship_id, Some(struct_id));

    ctx.dispatch_rpc(
        &mut sess,
        "structureControl",
        None,
        "ReleaseControl",
        &[PyRep::Integer(struct_id)],
        &kw,
        None,
    )
    .await
    .expect("ReleaseControl");

    assert_eq!(sess.ship_id, Some(ship_id));
    assert_eq!(ctx.db.get_character(char_id).unwrap().active_ship_id, ship_id);
}

#[tokio::test]
async fn test_r4_171_ess_concurrent_link_rejection_and_reserve_unlock_window() {
    let ctx = ServerContext::new();
    let mut sess1 = SessionState::new(17101);
    let mut sess2 = SessionState::new(17102);
    login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    let c1 = 90_000_001;
    let c2 = 90_000_002;
    let s1_id = 100_000_001;
    let s2_id = 100_000_002;
    let sys_id = 30_004_759u32; // Nullsec
    let kw = HashMap::new();

    ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(c1)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(c2)], &kw, None).await.unwrap();

    for (sess, cid, sid) in [(&mut sess1, c1, s1_id), (&mut sess2, c2, s2_id)] {
        sess.station_id = None;
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(sid);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.station_id = None;
                ch.solar_system_id = sys_id;
                ch.active_ship_id = sid;
            }
        });
    }

    let ess_ball_id = 950_171_000i64;
    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle
        .spawn_ball(eve_destiny::Ball {
            id: ess_ball_id,
            type_id: 32882, // ESS
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 1500.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    for sid in [s1_id, s2_id] {
        let _ = handle
            .spawn_ball(eve_destiny::Ball {
                id: sid,
                type_id: 587,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await;
    }

    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.ess_main_bank = IskCents::from_isk(10_000_000);
        st.ess_reserve_bank = IskCents::from_isk(30_000_000);
        // Give c1 a 15-min SW ESS Reserve Security Key (57720) in cargo for Delve (30_004_759)
        snap.items.insert(
            770_171_001,
            InventoryItemRecord {
                item_id: 770_171_001,
                type_id: 57720,
                owner_id: c1,
                location_id: s1_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // c1 links to Main Bank
    ctx.dispatch_rpc(
        &mut sess1,
        "essMgr",
        None,
        "AttemptLinkToMainBank",
        &[PyRep::Integer(ess_ball_id)],
        &kw,
        None,
    )
    .await
    .expect("c1 links to Main Bank");

    // c2 attempting to link to Main Bank while c1 is linked is rejected with EssAlreadyLinked
    let c2_main_err = ctx
        .dispatch_rpc(
            &mut sess2,
            "essMgr",
            None,
            "AttemptLinkToMainBank",
            &[PyRep::Integer(ess_ball_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(c2_main_err.code, "EssAlreadyLinked");

    // c1 links to Reserve Bank (consumes key 57714 and sets ess_reserve_unlocked_until)
    ctx.dispatch_rpc(
        &mut sess1,
        "essMgr",
        None,
        "AttemptLinkToReserveBank",
        &[PyRep::Integer(ess_ball_id)],
        &kw,
        None,
    )
    .await
    .expect("c1 links to Reserve Bank");
    let unlocked = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.ess_reserve_unlocked_until.get(&sys_id).copied().unwrap_or(0)
    });
    assert!(unlocked > ctx.now_unix(), "Reserve Bank unlock window must be populated");
}

#[tokio::test]
async fn test_r4_172_incursion_wave_respawn_attaches_combat_state_and_site_claim() {
    let ctx = Arc::new(ServerContext::new());
    let sys_id = 30_000_142u32;
    let site_id = "inc-r4-172-vg";
    let initial_boss_bid = 910_172_000i64;
    let boss_tid = 19720u32; // Sansha NPC type

    let now_u = ctx.now_unix();
    eve_server::services::pve_special_sites::with_pve_special_reg_db(&ctx.db, now_u, |reg| {
        reg.incursions.insert(
            20_000_020,
            eve_server::services::pve_incursion_sites::IncursionTaleRecord {
                tale_id: 20_000_020,
                constellation_id: 20_000_020,
                constellation_name: "Kimotoro".into(),
                staging_solar_system_id: sys_id,
                vanguard_solar_system_ids: vec![sys_id],
                assault_solar_system_ids: vec![sys_id],
                hq_solar_system_id: sys_id,
                influence: 0.5,
                state: "Established".into(),
                lockout_until_filetime: None,
                mothership_spawned: false,
                mothership_defeated: false,
                tale_stop_published: false,
                sites: vec![eve_server::services::pve_incursion_sites::IncursionSiteEntry {
                    site_id: site_id.into(),
                    dungeon_id: 172,
                    dungeon_name: "Sansha Vanguard".into(),
                    tier: "Vanguard".into(),
                    solar_system_id: sys_id,
                    position: glam::DVec3::new(100_000.0, 0.0, 0.0),
                    min_pilots: 1,
                    max_optimal_pilots: 10,
                    base_isk: 10_000_000,
                    base_concord_lp: 1_000,
                    influence_delta: 0.1,
                    locked_until_low_influence: false,
                    completed_count: 0,
                    boss_npc_ball_id: initial_boss_bid,
                    boss_type_id: boss_tid,
                    boss_destroyed: true,
                    killer_char_id: None,
                    killer_fleet_id: None,
                    escort_spawns: vec![],
                }],
            },
        );
    });

    let res = eve_server::services::pve_incursion_sites::process_incursion_site_completion_and_payout_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        None,
        None,
        sys_id,
        site_id,
        None,
    )
    .await
    .expect("complete incursion wave");
    let next_bid = initial_boss_bid + 1_000;
    assert_eq!(res.tier, "Vanguard");
    assert!(
        !ctx.systems.claim_special_site_ball(sys_id, next_bid),
        "next_bid must already be claimed as a special-site ball"
    );
    let handle = ctx.systems.get_or_spawn(sys_id);
    let cs = handle
        .get_combat_state(next_bid)
        .await
        .unwrap()
        .expect("next wave boss must have CombatState");
    assert!(cs.max_structure_hp > 0.0, "Respawned wave boss must have non-zero HP");
}

#[tokio::test]
async fn test_r4_173_join_faction_as_character_enforces_cooldown_standing_corp_and_rearguard_lp() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(17301);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let caldari = 500_001i64;
    let minmatar = 500_002i64;
    let kw = HashMap::new();

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap();

    // 1. Negative standing blocks JoinFactionAsCharacter
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(minmatar, -0.5);
    });
    let err_st = ctx
        .dispatch_rpc(&mut sess, "facWarMgr", None, "JoinFactionAsCharacter", &[PyRep::Integer(minmatar)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_st.code, "FwStandingTooLow");

    // 2. Enlist via fwCharacterEnlistmentMgr, retire (starts 24h cooldown), and verify JoinFactionAsCharacter is blocked by cooldown
    ctx.dispatch_rpc(&mut sess, "fwCharacterEnlistmentMgr", None, "CreateMyDirectEnlistment", &[PyRep::Integer(caldari)], &kw, None)
        .await
        .unwrap();
    ctx.dispatch_rpc(&mut sess, "fwCharacterEnlistmentMgr", None, "RemoveMyDirectEnlistment", &[], &kw, None)
        .await
        .unwrap();
    let err_cd = ctx
        .dispatch_rpc(&mut sess, "facWarMgr", None, "JoinFactionAsCharacter", &[PyRep::Integer(caldari)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_cd.code, "FwEnlistmentCooldown");

    // Advance past 24h cooldown and enlist in Minmatar (after fixing standing)
    ctx.advance_virtual_clock(86_405);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(minmatar, 1.0);
    });
    ctx.dispatch_rpc(&mut sess, "facWarMgr", None, "JoinFactionAsCharacter", &[PyRep::Integer(minmatar)], &kw, None)
        .await
        .expect("JoinFactionAsCharacter after cooldown");

    // 3. In a low-sec Rearguard FW system, completing CaptureComplex pays 0.75x * 15,000 = 11,250 LP (not 1.5x = 22,500 LP)
    let rearguard_sys = ctx.db.with_snapshot(|snap| {
        snap.fw_system_occupations
            .keys()
            .copied()
            .find(|&sid| {
                let sec = ctx.sde.solar_systems.get(&sid).map(|s| s.security_status).unwrap_or(1.0);
                sec > 0.0
                    && sec < 0.45
                    && matches!(snap.fw_system_occupations[&sid].occupier_faction_id, 500_001 | 500_003)
                    && eve_server::services::fw_services::compute_system_frontline_status(&ctx, snap, sid)
                        == eve_server::services::fw_services::STATUS_REARGUARD
            })
            .expect("must have at least one low-sec Rearguard FW system")
    });
    let ship_id = 100_000_001i64;
    sess.station_id = None;
    sess.solar_system_id = Some(rearguard_sys);
    sess.ship_id = Some(ship_id);
    let handle = ctx.systems.get_or_spawn(rearguard_sys);
    let _ = handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let _ = handle.step_manual(1.0).await;
    let militia_corp = ctx
        .sde
        .factions
        .get(&(minmatar as u32))
        .and_then(|f| f.militia_corporation_id)
        .map(|c| c as i64)
        .unwrap_or(minmatar);
    let (lp_before, vp_before) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.solar_system_id = rearguard_sys;
        }
        let lp = snap
            .characters
            .get(&char_id)
            .and_then(|c| c.loyalty_points.get(&militia_corp).copied())
            .unwrap_or(0);
        let vp = snap
            .fw_system_occupations
            .get(&rearguard_sys)
            .map(|o| o.victory_points)
            .unwrap_or(0);
        let st = snap.system_states.entry(rearguard_sys).or_default();
        st.solar_system_id = rearguard_sys;
        st.fw_complex_position = Some([0.0, 0.0, 0.0]);
        st.fw_capture_duration_seconds = 600;
        st.fw_capture_seconds_elapsed = 599;
        st.fw_contested_pct = 10.0;
        st.last_fw_capture_tick = 0;
        (lp, vp)
    });
    assert!(
        eve_server::services::insurgency_services::step_system_insurgency_capture_tick(
            &ctx,
            rearguard_sys,
            ship_id,
            minmatar,
        )
        .await,
        "1 Hz server capture tick must advance Rearguard FW complex"
    );
    ctx.db.with_snapshot(|snap| {
        let lp_after = snap.characters[&char_id].loyalty_points.get(&militia_corp).copied().unwrap_or(0);
        assert_eq!(lp_after - lp_before, 11_250);
        assert!(snap.system_states[&rearguard_sys].fw_complex_position.is_none());
        let occ = &snap.fw_system_occupations[&rearguard_sys];
        assert_eq!(occ.victory_points - vp_before, 20);
        let expected_pct = ((occ.victory_points as f64 / (occ.victory_points_threshold.max(1) as f64)) * 100.0).clamp(0.0, 100.0);
        assert!((snap.system_states[&rearguard_sys].fw_contested_pct - expected_pct).abs() < 1e-6);
    });
}

#[tokio::test]
async fn test_r4_174_cannot_mutate_fitted_module() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(17401);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let kw = HashMap::new();

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap();

    let fitted_ab_id = 770_174_001i64;
    let muta_id = 770_174_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fitted_ab_id,
            InventoryItemRecord {
                item_id: fitted_ab_id,
                type_id: 438, // 1MN Afterburner II
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            muta_id,
            InventoryItemRecord {
                item_id: muta_id,
                type_id: 47746, // Decayed 1MN Afterburner Mutaplasmid
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "dynamicItemService",
            None,
            "CreateDynamicItem",
            &[PyRep::Integer(47746), PyRep::Integer(fitted_ab_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "CannotMutateFittedItem");
}

#[tokio::test]
async fn test_r4_175_poco_tax_syncs_corp_wallet_division_zero() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(17501);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let corp_id = 98_175_001i64;
    let planet_id = 40_009_082i64;
    let ship_id = 100_000_001i64;
    let kw = HashMap::new();

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap();
    sess.station_id = None;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(10_000_000);
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "POCO 175 Corp".into(),
                ticker: "P175".into(),
                ceo_id: char_id,
                members: HashSet::from([char_id]),
                master_wallet: IskCents(0),
                wallet_divisions: vec![],
                ..Default::default()
            },
        );
        // Seed command center on planet 40009082
        snap.items.insert(
            770_175_001,
            InventoryItemRecord {
                item_id: 770_175_001,
                type_id: 2534,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // Configure player POCO owned by corp_id with 10% tax rate
        let settings = eve_server::services::industry_pi::pi_helpers::OrbitalCustomsSettings {
            planet_id,
            orbital_type_id: 2233,
            owner_corp_id: corp_id,
            corporation_tax_rate: 0.10,
            alliance_tax_rate: 0.10,
            standing_high_tax_rate: 0.10,
            standing_good_tax_rate: 0.10,
            standing_neutral_tax_rate: 0.10,
            standing_bad_tax_rate: 0.10,
            standing_horrible_tax_rate: 0.10,
            allow_alliance: true,
            allow_standings: true,
            minimum_standing: -10.0,
        };
        eve_server::services::industry_pi::pi_helpers::save_orbital_settings(snap, planet_id, &settings);
    });

    ctx.dispatch_rpc(
        &mut sess,
        "planetMgr",
        None,
        "UserUpdateNetwork",
        &[
            PyRep::Integer(planet_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(2534),
                PyRep::Real(0.1),
                PyRep::Real(0.2),
            ])]),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();

    // Place 100x Water (3645) in POCO and import to colony
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            770_175_002,
            InventoryItemRecord {
                item_id: 770_175_002,
                type_id: 3645,
                owner_id: char_id,
                location_id: planet_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    ctx.dispatch_rpc(
        &mut sess,
        "planetMgr",
        None,
        "UserLaunchCommodities",
        &[
            PyRep::Integer(planet_id),
            PyRep::Integer(3645),
            PyRep::Integer(100),
            PyRep::String("Import".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("UserLaunchCommodities Import");

    let (master_bal, div0_bal) = ctx.db.with_snapshot(|snap| {
        let corp = &snap.corporations[&corp_id];
        (
            corp.master_wallet.0,
            corp.wallet_divisions.first().map(|d| d.balance.0).unwrap_or(-1),
        )
    });
    assert!(master_bal > 0, "Corp master_wallet must be credited with POCO tax");
    assert_eq!(div0_bal, master_bal, "Corp wallet_divisions[0].balance must match master_wallet");
}

}
