//! Upwell structure lifecycle tests (`structure_lifecycle` engine + `structureDeployment.Anchor`,
//! `structureControl.FuelStructure`, `structureControl.AttackStructure`, `structureDeployment.Unanchor`).
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
const JITA: u32 = 30_000_142;
const RIVAL_CORP: i64 = 98_999_999;

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 {
    match rep {
        PyRep::KeyVal(e) => e.get(key).and_then(|v| v.as_i64()).unwrap_or(0),
        _ => 0,
    }
}

fn kv_str(rep: &PyRep, key: &str) -> String {
    match rep {
        PyRep::KeyVal(e) => e.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string(),
        _ => String::new(),
    }
}

fn grant(ctx: &ServerContext, owner: i64, location: i64, type_id: u32, flag: InventoryFlag, qty: i32) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id,
                owner_id: owner,
                location_id: location,
                flag,
                quantity: qty,
                singleton: qty == 1,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        id
    })
}

fn attr(ctx: &ServerContext, type_id: u32, attr_id: u16) -> f64 {
    ctx.sde.types.get(&type_id).and_then(|t| t.default_attributes.get(&attr_id).copied()).unwrap_or_else(|| panic!("SDE type {type_id} lacks attribute {attr_id}"))
}

/// Logged-in pilot with a player corp, undocked in Jita and moved far from stations/gates.
async fn pilot_in_space(ctx: &ServerContext, sess_id: u64) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)]).await.expect("Select");
    rpc(ctx, &mut sess, "corpRegistry", "AddCorporation", &[PyRep::String("Lifecycle Holdings".into()), PyRep::String("LIFE".into())]).await.expect("AddCorporation");
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
    rpc(ctx, sess, "structureDocking", "Dock", &[PyRep::Integer(id)]).await.expect("Dock");
    let core_item = grant(ctx, OWNER, sess.ship_id.expect("ship"), core, InventoryFlag::Cargo, 1);
    rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(core_item), PyRep::Integer(id), PyRep::Integer(4)]).await.expect("move core into structure");
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
    let sys = ctx.systems.get_or_spawn(sess.solar_system_id.expect("sys"));
    for _ in 0..12 {
        let _ = sys.step_manual(1.0).await;
    }
    // rpc-lint: allow S-7 in action_plan.md names AttackStructure; exercises structure_lifecycle::apply_structure_damage
    rpc(ctx, sess, "structureControl", "AttackStructure", &[PyRep::Integer(id)]).await
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
    rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(id)]).await.expect("Dock");
    let wrong_core = attr(&ctx, 35834, life::ATTR_REQUIRES_DEED_TYPE) as u32;
    let wrong = grant(&ctx, OWNER, ship, wrong_core, InventoryFlag::Cargo, 1);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(wrong), PyRep::Integer(id), PyRep::Integer(4)]).await.expect("move wrong core");
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().state, life::STATE_NO_CORE);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(spare_core), PyRep::Integer(id), PyRep::Integer(4)]).await.expect("move core");
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
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&core_drop).expect("dropped core");
        assert_eq!(it.type_id, core_type);
        assert_eq!(it.location_id, i64::from(JITA));
        assert_eq!(snap.item_positions.get(&core_drop), Some(&st.position));
    });
}

#[tokio::test]
async fn test_fuel_consumption_low_power_and_abandoned() {
    // parity: feat:structures.fuel-consumption, feat:structures.abandoned
    eve_testkit::attest(&["feat:structures.fuel-consumption", "feat:structures.abandoned"]);
    let ctx = ServerContext::new();
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

    // Only SDE fuel-group items are accepted as fuel.
    let ship = sess.ship_id.expect("ship");
    grant(&ctx, OWNER, ship, 34, InventoryFlag::Cargo, 5_000);
    // rpc-lint: allow pre-existing FuelStructure handler (S-7 / regressions)
    let bad = rpc(&ctx, &mut sess, "structureControl", "FuelStructure", &[PyRep::Integer(id), PyRep::Integer(1_000)]).await.expect_err("tritanium is not fuel");
    assert_eq!(bad.code, "InsufficientFuelBlocks");
    let load = online_need + 30 * per_hour;
    grant(&ctx, OWNER, ship, fuel_type, InventoryFlag::Cargo, load as i32);
    // rpc-lint: allow pre-existing FuelStructure handler (S-7 / regressions)
    let fueled = rpc(&ctx, &mut sess, "structureControl", "FuelStructure", &[PyRep::Integer(id), PyRep::Integer(i64::from(load))]).await.expect("FuelStructure");
    assert_eq!(kv_i64(&fueled, "fuelBlocks"), i64::from(load));

    // Now the module comes online -> full power, shield multiplied per SDE full-power attributes.
    ctx.db.with_mut_snapshot(|snap| snap.module_online_states.insert(svc, true));
    let st = observe(&ctx, &mut sess, id).await.unwrap();
    assert!(module_online(&ctx, svc));
    assert_eq!(power(&ctx, id), life::PowerState::FullPower);
    let base_shield = attr(&ctx, 35832, life::ATTR_SHIELD_CAPACITY);
    let full_shield = base_shield * attr(&ctx, 35832, life::ATTR_STRUCTURE_FULL_POWER_HP_MULT) * attr(&ctx, clone_bay, life::ATTR_SERVICE_FULL_POWER_HP_MULT);
    assert!(st.shield_hp <= base_shield + 1e-6);

    // Hourly burn per online service module.
    ctx.advance_virtual_clock(3 * 3_600);
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().fuel_blocks, load - 3 * per_hour);
    // A second online module adds its own hourly amount (Standup Manufacturing Plant I).
    let factory = 35878u32;
    let factory_hour = attr(&ctx, factory, life::ATTR_SERVICE_FUEL_PER_HOUR) as u32;
    let factory_item = grant(&ctx, OWNER, id, factory, InventoryFlag::ServiceSlot1, 1);
    ctx.db.with_mut_snapshot(|snap| snap.module_online_states.insert(factory_item, true));
    observe(&ctx, &mut sess, id).await;
    assert!(module_online(&ctx, factory_item));
    ctx.advance_virtual_clock(3_600);
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().fuel_blocks, load - 4 * per_hour - factory_hour);
    ctx.db.with_mut_snapshot(|snap| snap.module_online_states.insert(factory_item, false));
    let load = load - per_hour - factory_hour;
    ctx.advance_virtual_clock(1_800);
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().fuel_blocks, load - 3 * per_hour, "no partial-hour burn");
    // A repair to full happens at full-power shield (damage it a little and let it repair).
    set_hp(&ctx, id, 1.0, attr(&ctx, 35832, life::ATTR_ARMOR_HP), attr(&ctx, 35832, life::ATTR_HP));
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(id).or_default().repair_until_unix = Some(ctx.now_unix());
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });
    assert!((observe(&ctx, &mut sess, id).await.unwrap().shield_hp - full_shield).abs() < 1e-6);

    // Run dry: when an hour cannot be paid the module goes offline -> low power, shield clamped.
    let remaining_hours = u64::from((load - 3 * per_hour) / per_hour);
    ctx.advance_virtual_clock((remaining_hours + 1) * 3_600);
    let st = observe(&ctx, &mut sess, id).await.unwrap();
    assert!(!module_online(&ctx, svc), "services offline when fuel runs out");
    assert_eq!(power(&ctx, id), life::PowerState::LowPower);
    assert!(st.shield_hp <= base_shield + 1e-6, "low power shield is the weaker SDE base");
    assert!(st.fuel_blocks < per_hour);

    // Seven days of low power -> abandoned.
    ctx.advance_virtual_clock(life::ABANDON_AFTER_LOW_POWER_SECS - 3 * 3_600);
    observe(&ctx, &mut sess, id).await;
    assert_eq!(power(&ctx, id), life::PowerState::LowPower);
    ctx.advance_virtual_clock(3 * 3_600);
    observe(&ctx, &mut sess, id).await;
    assert_eq!(power(&ctx, id), life::PowerState::Abandoned);

    // Abandoned = single timer: no armor or hull reinforcement, destroyed in one fight.
    prepare_siege(&ctx, id);
    set_hp(&ctx, id, 1.0, 1.0, 1.0);
    let a = attack(&ctx, &mut sess, id).await.expect("shield");
    assert_eq!(kv_str(&a, "state"), life::STATE_ARMOR_VULNERABLE);
    let b = attack(&ctx, &mut sess, id).await.expect("armor");
    assert_eq!(kv_str(&b, "state"), life::STATE_HULL_VULNERABLE);
    let c = attack(&ctx, &mut sess, id).await.expect("hull");
    assert_eq!(kv_str(&c, "state"), life::STATE_DESTROYED);
}

#[tokio::test]
async fn test_low_power_large_structure_skips_armor_reinforcement() {
    // parity: feat:structures.low-power
    eve_testkit::attest(&["feat:structures.low-power"]);
    let ctx = ServerContext::new();
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
    let per_hour = attr(&ctx, 35894, life::ATTR_SERVICE_FUEL_PER_HOUR) as u32;
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
    ctx.advance_virtual_clock(u64::from(online_need / per_hour + 1) * 3_600);
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
    assert_eq!(kv_i64(&r, "reinforcedUntilUnix") as u64, exit);
    assert_eq!(exit % 86_400 / 3_600, hour);
    let inv = attack(&ctx, &mut sess, med).await.expect_err("reinforced");
    assert_eq!(inv.code, "StructureReinforcedInvulnerable");
    ctx.advance_virtual_clock(exit - ctx.now_unix());
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
    let now = ctx.now_unix();
    let r = attack(&ctx, &mut sess, large).await.expect("large shield");
    assert_eq!(kv_i64(&r, "reinforcedUntilUnix") as u64, life::next_reinforce_exit(now + 24 * 3_600, lhour));
    ctx.advance_virtual_clock(kv_i64(&r, "reinforcedUntilUnix") as u64 - ctx.now_unix());
    assert_eq!(observe(&ctx, &mut sess, large).await.unwrap().state, life::STATE_ARMOR_VULNERABLE);
    set_hp(&ctx, large, 0.0, 1.0, attr(&ctx, 35833, life::ATTR_HP));
    let now = ctx.now_unix();
    let r = attack(&ctx, &mut sess, large).await.expect("large armor");
    assert_eq!(kv_str(&r, "state"), life::STATE_HULL_REINFORCED);
    assert_eq!(kv_i64(&r, "reinforcedUntilUnix") as u64, life::next_reinforce_exit(now + 108 * 3_600, lhour));
}

#[tokio::test]
async fn test_damage_cap_and_repair_timer_with_pause_threshold() {
    // parity: feat:structures.damage-cap, feat:structures.repair-timer
    eve_testkit::attest(&["feat:structures.damage-cap", "feat:structures.repair-timer"]);
    let ctx = ServerContext::new();
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
    ctx.db.with_snapshot(|snap| assert_eq!(snap.items.get(&core_drop).map(|i| (i.type_id, i.location_id)), Some((core_type, i64::from(JITA)))));
    let km = kv_i64(&r, "killmailID");
    let wraps: Vec<i64> = match &r {
        PyRep::KeyVal(e) => match e.get("assetSafetyWrapIDs") {
            Some(PyRep::List(l)) => l.iter().filter_map(|v| v.as_i64()).collect(),
            _ => vec![],
        },
        _ => vec![],
    };
    let expected_station = ctx.sde.stations.values().filter(|s| s.solar_system_id == JITA).map(|s| s.station_id).min();
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
        snap.structures.insert(wh_id, UpwellStructureRecord { structure_id: wh_id, type_id: 35832, owner_corp_id: RIVAL_CORP, solar_system_id: wh_sys, name: "J-space Astrahus".into(), position: [1.0, 2.0, 3.0], state: life::STATE_HULL_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 0, shield_hp: 0.0, armor_hp: 0.0, hull_hp: 0.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0 });
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
    eve_testkit::attest(&["feat:structures.unanchor"]);
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
