//! Integration tests for Upwell structure deployment, quantum cores, renaming,
//! structure types, fuel consumption, abandoned state, service modules, market,
//! clone bay, and reprocessing services.
//!
//! Parity rows:
//! - `feat:structures.deploy-anchor`
//! - `feat:structures.quantum-core`
//! - `feat:structures.rename`
//! - `feat:structures.structure-types`
//! - `feat:structures.fuel-consumption`
//! - `feat:structures.abandoned`
//! - `feat:structures.service-modules`
//! - `feat:structures.service-market`
//! - `feat:structures.service-clone-bay`
//! - `feat:structures.service-reprocessing`

#![forbid(unsafe_code)]

use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::services::station_structure::structure_upwell_ops::{load_ext_state, structure_allows_market_orders};
use eve_server::services::structure_acl_defense_rules as rules;
use glam::DVec3;
use std::collections::HashMap;

const OWNER: i64 = 90_000_001;
const JITA: u32 = 30_000_144;
const RIVAL_CORP: i64 = 98_999_999;

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 { rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or(0) }
fn kv_str(rep: &PyRep, key: &str) -> String { rep.dict_get(key).and_then(|v| v.as_str()).unwrap_or("").to_string() }
fn kv_bool(rep: &PyRep, key: &str) -> bool { rep.dict_get(key).and_then(|v| v.as_bool()).unwrap_or(false) }

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

async fn pilot_in_space(ctx: &ServerContext, sess_id: u64, char_id: i64) -> SessionState {
    let perim_station = ctx.sde.stations.values().find(|s| s.solar_system_id == JITA).map(|s| s.station_id).expect("Perimeter station");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            let ship_id = ch.active_ship_id;
            ch.solar_system_id = JITA;
            ch.station_id = Some(perim_station);
            ch.balance = IskCents::from_isk(500_000_000);
            if let Some(sh) = snap.items.get_mut(&ship_id) { sh.location_id = perim_station; }
        }
    });
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("Select");
    ctx.advance_secs(12);
    if char_id == OWNER {
        rpc(ctx, &mut sess, "corpRegistry", "AddCorporation", &[PyRep::String("Upwell Engineering Co".into()), PyRep::String("UPCO".into())]).await.expect("AddCorp");
    } else {
        let corp_id = ctx.db.with_snapshot(|s| s.characters[&OWNER].corporation_id);
        ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&char_id) { ch.corporation_id = corp_id; } });
        sess.corporation_id = Some(corp_id);
    }
    ctx.advance_virtual_clock(7 * 86_400 + 12);
    rpc(ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    sess
}

async fn move_ship(ctx: &ServerContext, sess: &SessionState, pos: DVec3) {
    assert!(ctx.systems.get_or_spawn(sess.solar_system_id.expect("sys")).set_ball_position(sess.ship_id.expect("ship"), pos).await);
}

fn open_space(ctx: &ServerContext, k: f64) -> DVec3 {
    let st = ctx.sde.stations.values().filter(|s| s.solar_system_id == JITA).map(|s| s.position).next().expect("Jita station");
    st + DVec3::new(0.0, 5.0e9 + k * 3.0e9, 0.0)
}

async fn anchor(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, name: &str) -> Result<PyRep, BridgeRpcError> {
    let ship = sess.ship_id.expect("ship");
    grant(ctx, OWNER, ship, type_id, InventoryFlag::Cargo, 1);
    rpc(ctx, sess, "structureDeployment", "Anchor", &[PyRep::Integer(i64::from(type_id)), PyRep::String(name.into())]).await
}

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

async fn online_structure(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, name: &str, offset: f64) -> i64 {
    if sess.structure_id.is_some() || sess.station_id.is_some() {
        rpc(ctx, sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    }
    move_ship(ctx, sess, open_space(ctx, offset)).await;
    let id = kv_i64(&anchor(ctx, sess, type_id, name).await.expect("Anchor"), "structureID");
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS);
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_NO_CORE);
    let core = attr(ctx, type_id, life::ATTR_REQUIRES_DEED_TYPE) as u32;
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

fn module_online(ctx: &ServerContext, item: i64) -> bool {
    ctx.db.with_snapshot(|snap| snap.module_online_states.get(&item).copied().unwrap_or(false))
}

fn prepare_siege(ctx: &ServerContext, id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let my_corp = snap.characters.get(&OWNER).map(|c| c.corporation_id).expect("corp");
        if let Some(s) = snap.structures.get_mut(&id) { s.owner_corp_id = RIVAL_CORP; }
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
            s.shield_hp = shield; s.armor_hp = armor; s.hull_hp = hull;
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
    let act = rpc(ctx, sess, "dogmaIM", "Activate", &[PyRep::Integer(100_000_005), PyRep::String("projectileFired".into()), PyRep::Integer(id)]).await?;
    Ok(ctx.db.with_snapshot(|s| {
        let st = s.structures.get(&id).expect("structure");
        let core_tid = ctx.sde.types.get(&st.type_id).and_then(|t| t.default_attributes.get(&life::ATTR_REQUIRES_DEED_TYPE)).map(|v| *v as u32).unwrap_or(0);
        let wreck_id = s.items.values().find(|i| i.location_id == i64::from(sys_id) && s.item_positions.get(&i.item_id) == Some(&st.position) && (i.type_id == 26557 || ctx.sde.wrecks.values().any(|&w| w == i.type_id))).map(|i| i.item_id);
        let core_drop = s.items.values().filter(|i| i.type_id == core_tid && (i.location_id == i64::from(sys_id) || Some(i.location_id) == wreck_id)).map(|i| i.item_id).max().unwrap_or(0);
        PyRep::keyval([("state", PyRep::String(st.state.clone())), ("killmailID", PyRep::Integer(kv_i64(&act, "killID"))), ("droppedQuantumCoreItemID", PyRep::Integer(core_drop))])
    }))
}

#[tokio::test]
async fn test_structure_deployment_quantum_core_types_and_rename() {
    // parity: feat:structures.deploy-anchor
    // parity: feat:structures.quantum-core
    // parity: feat:structures.rename
    // parity: feat:structures.structure-types
    eve_testkit::attest(&[
        "feat:structures.deploy-anchor",
        "feat:structures.quantum-core",
        "feat:structures.rename",
        "feat:structures.structure-types",
    ]);

    let ctx = ServerContext::new();
    let clock = std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000));
    ctx.set_clock(clock);
    let mut sess = pilot_in_space(&ctx, 6101, OWNER).await;

    // 1. Structure types & SDE profiles across Citadels, Complexes, Refineries & Faction Fortizars
    for (tid, name, exp_sh, exp_ar, exp_hu, hi, med, low, svc, rig) in [
        (35832u32, "Astrahus", 3600000.0, 1800000.0, 1800000.0, 4, 4, 3, 3, 3),
        (35833, "Fortizar", 6750000.0, 5400000.0, 21600000.0, 6, 5, 4, 5, 3),
        (35834, "Keepstar", 33750000.0, 27000000.0, 108000000.0, 8, 6, 5, 7, 3),
        (35825, "Raitaru", 2400000.0, 1200000.0, 1200000.0, 3, 2, 1, 3, 3),
        (35826, "Azbel", 4500000.0, 3600000.0, 14400000.0, 4, 3, 2, 5, 3),
        (35827, "Sotiyo", 22500000.0, 18000000.0, 72000000.0, 6, 4, 3, 6, 3),
        (35835, "Athanor", 2400000.0, 1200000.0, 1200000.0, 3, 3, 1, 3, 3),
        (35836, "Tatara", 4500000.0, 3600000.0, 14400000.0, 5, 4, 3, 5, 3),
    ] {
        let p = life::structure_profile(&ctx, tid).expect(name);
        assert_eq!((p.shield_hp, p.armor_hp, p.hull_hp), (exp_sh, exp_ar, exp_hu));
        assert_eq!((p.high_slots, p.mid_slots, p.low_slots, p.service_slots, p.rig_slots), (hi, med, low, svc, rig));
    }
    let raitaru_p = life::structure_profile(&ctx, 35825).unwrap();
    assert!(raitaru_p.eng_time_bonus_pct != 0.0 && raitaru_p.eng_mat_bonus_pct != 0.0);
    assert_eq!((life::structure_profile(&ctx, 35835).unwrap().refining_yield_bonus, life::structure_profile(&ctx, 35836).unwrap().refining_yield_bonus), (2.0, 5.5));
    for fid in 47512..=47516 {
        let fp = life::structure_profile(&ctx, fid).expect("faction fortizar");
        assert_eq!((fp.hull_class, fp.high_slots, fp.rig_slots), (life::HullClass::Large, 7, 3));
        assert!(fp.fuel_bonus_pct < 0.0 && (7..=8).contains(&fp.service_slots));
    }
    let palatine = life::structure_profile(&ctx, 40340).expect("palatine");
    assert_eq!((palatine.hull_class, palatine.fuel_bonus_pct, palatine.service_slots), (life::HullClass::XLarge, -25.0, 8));

    // 2. Deployment distance rules: refused close to station or HighSec Keepstar
    let near = anchor(&ctx, &mut sess, 35832, "Near Station").await.expect_err("near station");
    assert_eq!(near.code, "TooCloseToStructureOrStation");
    move_ship(&ctx, &sess, open_space(&ctx, 0.0)).await;
    let ks = anchor(&ctx, &mut sess, 35834, "HighSec Keepstar").await.expect_err("highsec keepstar");
    assert_eq!(ks.code, "CannotAnchorInHighSec");

    // 3. Successful Anchor: 15-minute AnchorVulnerable, core required
    let ship = sess.ship_id.unwrap();
    let core_type = attr(&ctx, 35832, life::ATTR_REQUIRES_DEED_TYPE) as u32;
    let spare_core = grant(&ctx, OWNER, ship, core_type, InventoryFlag::Cargo, 1);
    let res = anchor(&ctx, &mut sess, 35832, "Prometheus Astrahus").await.expect("Anchor");
    let id = kv_i64(&res, "structureID");
    assert_eq!(kv_str(&res, "state"), life::STATE_ANCHOR_VULNERABLE);
    assert_eq!(kv_i64(&res, "quantumCoreTypeID"), i64::from(core_type));
    let st = observe(&ctx, &mut sess, id).await.expect("structure");
    assert!(!st.quantum_core_installed);
    assert_eq!((st.shield_hp, st.armor_hp, st.hull_hp), (0.0, 0.0, attr(&ctx, 35832, life::ATTR_HP)));
    let overlap = anchor(&ctx, &mut sess, 35832, "Overlap").await.expect_err("overlap");
    assert_eq!(overlap.code, "TooCloseToStructureOrStation");

    // 4. 15m vulnerability -> 24h Anchoring -> NoCore
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS);
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().state, life::STATE_ANCHORING);
    ctx.advance_virtual_clock(life::ANCHORING_SECS);
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().state, life::STATE_NO_CORE);
    ctx.advance_virtual_clock(3 * 86_400);
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().state, life::STATE_NO_CORE, "cannot online without core");

    // 5. Install Quantum Core -> Onlining (15m) -> ShieldVulnerable
    move_ship(&ctx, &sess, DVec3::from_array(ctx.db.with_snapshot(|s| s.structures[&id].position))).await;
    rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(id)]).await.expect("Dock");
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(spare_core), PyRep::Integer(id), PyRep::Integer(172)]).await.expect("insert core");
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().state, life::STATE_ONLINING);
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    ctx.advance_virtual_clock(life::ONLINING_SECS);
    let st_on = observe(&ctx, &mut sess, id).await.unwrap();
    assert_eq!(st_on.state, life::STATE_SHIELD_VULNERABLE);
    assert!(st_on.quantum_core_installed);

    // 6. RenameStructure persists and visible in structureDirectory
    let blank = rpc(&ctx, &mut sess, "structureDeployment", "RenameStructure", &[PyRep::Integer(id), PyRep::String("   ".into())]).await.expect_err("blank name");
    assert_eq!(blank.code, "InvalidStructureName");
    let renamed = rpc(&ctx, &mut sess, "structureDeployment", "RenameStructure", &[PyRep::Integer(id), PyRep::String("Astrahus Citadel Prime".into())]).await.expect("rename");
    assert_eq!(kv_str(&renamed, "name"), "Astrahus Citadel Prime");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&id].name, "Astrahus Citadel Prime");
        assert_eq!(snap.item_custom_names.get(&id).map(String::as_str), Some("Astrahus Citadel Prime"));
    });
    let dir_info = rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(id)]).await.expect("GetStructureInfo");
    assert_eq!(kv_str(&dir_info, "itemName"), "Astrahus Citadel Prime");

    // 7. Core drops on destruction
    prepare_siege(&ctx, id);
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&id).unwrap().state = life::STATE_HULL_VULNERABLE.into());
    set_hp(&ctx, id, 0.0, 0.0, 1.0);
    let kill = attack(&ctx, &mut sess, id).await.expect("destruction");
    assert_eq!(kv_str(&kill, "state"), life::STATE_DESTROYED);
    let core_drop = kv_i64(&kill, "droppedQuantumCoreItemID");
    assert!(core_drop > 0, "quantum core must drop on structure destruction");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&core_drop].type_id, core_type);
    });
}

#[tokio::test]
async fn test_structure_fuel_consumption_abandoned_and_service_modules() {
    // parity: feat:structures.fuel-consumption
    // parity: feat:structures.abandoned
    // parity: feat:structures.service-modules
    eve_testkit::attest(&[
        "feat:structures.fuel-consumption",
        "feat:structures.abandoned",
        "feat:structures.service-modules",
    ]);

    let ctx = ServerContext::new();
    let clock = std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000));
    ctx.set_clock(clock);
    let mut sess = pilot_in_space(&ctx, 6102, OWNER).await;
    move_ship(&ctx, &sess, open_space(&ctx, 1.0)).await;
    let id = online_structure(&ctx, &mut sess, 35832, "Service Astrahus", 1.0).await;

    let clone_bay = 35894u32;
    let per_hour = attr(&ctx, clone_bay, life::ATTR_SERVICE_FUEL_PER_HOUR) as u32;
    let fuel_group = attr(&ctx, clone_bay, life::ATTR_SERVICE_FUEL_GROUP) as u32;
    let fuel_type = ctx.sde.types.values().filter(|t| t.group_id == fuel_group && t.published).map(|t| t.type_id).min().unwrap();

    // 1. Service module offline without fuel; fuel bay rejects invalid items
    move_ship(&ctx, &sess, DVec3::from_array(ctx.db.with_snapshot(|s| s.structures[&id].position))).await;
    rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(id)]).await.expect("dock");
    let ship = sess.ship_id.unwrap();
    let svc = grant(&ctx, OWNER, ship, clone_bay, InventoryFlag::Cargo, 1);
    let mut kw = HashMap::new();
    kw.insert("online".into(), PyRep::Bool(false));
    let fit_res = ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(svc), PyRep::Integer(id), PyRep::Integer(164)], &kw, None).await.expect("fit svc");
    let svc = kv_i64(&fit_res, "itemID");
    assert!(!module_online(&ctx, svc));
    assert_eq!(power(&ctx, id), life::PowerState::LowPower);
    let no_fuel = rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(id), PyRep::Integer(svc)]).await.expect_err("no fuel");
    assert_eq!(no_fuel.code, "InsufficientStructureFuelToOnline");

    let bad_item = grant(&ctx, OWNER, ship, 34, InventoryFlag::Cargo, 100);
    let bad_add = rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(bad_item), PyRep::Integer(id), PyRep::Integer(133), PyRep::Integer(100)]).await.expect_err("non-fuel");
    assert_eq!(bad_add.code, "InvalidInventoryFlag");

    // 2. Deposit fuel, online module, deduct startup fuel, enter FullPower
    let svc_online = rules::service_module_online_fuel_cost(&ctx, 35832, clone_bay);
    let factory = 35878u32;
    let factory_online = rules::service_module_online_fuel_cost(&ctx, 35832, factory);
    let burn_h = (f64::from(per_hour) * 0.75).round() as u32;
    let factory_hour = attr(&ctx, factory, life::ATTR_SERVICE_FUEL_PER_HOUR) as u32;
    let rem = (40 * per_hour - factory_hour) % burn_h;
    let load = svc_online + factory_online + 40 * per_hour - rem;
    let fuel_item = grant(&ctx, OWNER, ship, fuel_type, InventoryFlag::Cargo, load as i32);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(fuel_item), PyRep::Integer(id), PyRep::Integer(133), PyRep::Integer(i64::from(load))]).await.expect("fuel bay add");
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().fuel_blocks, load);

    let _ = ctx.drain_notifications_for_char(OWNER);
    let on_res = rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(id), PyRep::Integer(svc)]).await.expect("OnlineModule");
    assert!(kv_bool(&on_res, "online"));
    let st = observe(&ctx, &mut sess, id).await.unwrap();
    assert_eq!(st.fuel_blocks, load - svc_online);
    assert!(module_online(&ctx, svc));
    assert_eq!(power(&ctx, id), life::PowerState::FullPower);
    assert_eq!(rpc(&ctx, &mut sess, "structureSettings", "CharacterHasService", &[PyRep::Integer(id), PyRep::Integer(10)]).await.expect("has service"), PyRep::Bool(true));
    assert!(ctx.drain_notifications_for_char(OWNER).into_iter().any(|n| n.event == "OnStructureServiceChanged" && n.args == vec![PyRep::Integer(id)]));

    // 3. Hourly burn per online service module
    ctx.advance_virtual_clock(3 * 3_600);
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().fuel_blocks, load - svc_online - 3 * burn_h);

    let fact_item = grant(&ctx, OWNER, ship, factory, InventoryFlag::Cargo, 1);
    let fit_fact = ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(fact_item), PyRep::Integer(id), PyRep::Integer(165)], &kw, None).await.expect("fit factory");
    let fact_id = kv_i64(&fit_fact, "itemID");
    rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(id), PyRep::Integer(fact_id)]).await.expect("online factory");
    let both_burn = burn_h + factory_hour;
    ctx.advance_virtual_clock(3_600);
    let cur_blocks = load - svc_online - factory_online - 3 * burn_h - both_burn;
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().fuel_blocks, cur_blocks);

    rpc(&ctx, &mut sess, "dogmaIM", "TakeModuleOffline", &[PyRep::Integer(id), PyRep::Integer(fact_id)]).await.expect("offline factory");
    ctx.advance_virtual_clock(1_800);
    assert_eq!(observe(&ctx, &mut sess, id).await.unwrap().fuel_blocks, cur_blocks, "no partial hour burn");

    // 4. Run dry -> LowPower state and service modules offline
    let rem_hours = u64::from(cur_blocks / burn_h);
    ctx.advance_virtual_clock((rem_hours + 1) * 3_600);
    let st_dry = observe(&ctx, &mut sess, id).await.unwrap();
    assert_eq!(st_dry.fuel_blocks, 0);
    assert!(!module_online(&ctx, svc));
    assert_eq!(power(&ctx, id), life::PowerState::LowPower);
    assert_eq!(rpc(&ctx, &mut sess, "structureSettings", "CharacterHasService", &[PyRep::Integer(id), PyRep::Integer(10)]).await.expect("has service"), PyRep::Bool(false));

    // 5. Seven days of low power -> Abandoned state with single timer
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("undock");
    ctx.advance_virtual_clock(life::ABANDON_AFTER_LOW_POWER_SECS - 3_600);
    observe(&ctx, &mut sess, id).await;
    assert_eq!(power(&ctx, id), life::PowerState::LowPower);
    ctx.advance_virtual_clock(3_600);
    observe(&ctx, &mut sess, id).await;
    assert_eq!(power(&ctx, id), life::PowerState::Abandoned);

    let cargo_id = grant(&ctx, OWNER, id, 34, InventoryFlag::Hangar, 500);
    prepare_siege(&ctx, id);
    set_hp(&ctx, id, 1.0, 1.0, 1.0);
    let s = attack(&ctx, &mut sess, id).await.expect("shield");
    assert_eq!(kv_str(&s, "state"), life::STATE_ARMOR_VULNERABLE);
    let a = attack(&ctx, &mut sess, id).await.expect("armor");
    assert_eq!(kv_str(&a, "state"), life::STATE_HULL_VULNERABLE);
    let h = attack(&ctx, &mut sess, id).await.expect("hull");
    assert_eq!(kv_str(&h, "state"), life::STATE_DESTROYED);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&cargo_id).map(|i| i.location_id), Some(i64::from(JITA)), "abandoned structure drops hangar loot directly into space");
    });
}

#[tokio::test]
async fn test_structure_market_clone_bay_and_reprocessing_services() {
    // parity: feat:structures.service-market
    // parity: feat:structures.service-clone-bay
    // parity: feat:structures.service-reprocessing
    eve_testkit::attest(&[
        "feat:structures.service-market",
        "feat:structures.service-clone-bay",
        "feat:structures.service-reprocessing",
    ]);

    let ctx = ServerContext::new();
    let clock = std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000));
    ctx.set_clock(clock);
    let mut sess = pilot_in_space(&ctx, 6103, OWNER).await;
    move_ship(&ctx, &sess, open_space(&ctx, 2.0)).await;

    // 1. Market Service: Fortizar (Large) vs Astrahus (Medium)
    let fort_id = online_structure(&ctx, &mut sess, 35833, "Market Fortizar", 2.0).await;
    let ast_id = online_structure(&ctx, &mut sess, 35832, "Market Astrahus", 3.0).await;
    let ship_id = sess.ship_id.unwrap();
    let trit_item = grant(&ctx, OWNER, fort_id, 34, InventoryFlag::Hangar, 10_000);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.structure_id = Some(fort_id); ch.station_id = None;
        }
    });
    sess.structure_id = Some(fort_id);
    sess.station_id = None;
    sess.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_TRADER;

    // Offline market hub rejects orders
    let sell_entry = PyRep::List(vec![PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(trit_item)),
        (PyRep::String("typeID".into()), PyRep::Integer(34)),
        (PyRep::String("price".into()), PyRep::Real(100.0)),
        (PyRep::String("quantity".into()), PyRep::Integer(100)),
        (PyRep::String("stationID".into()), PyRep::Integer(fort_id)),
        (PyRep::String("duration".into()), PyRep::Integer(14)),
    ])]);
    let no_hub_err = rpc(&ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder", &[sell_entry.clone(), PyRep::Bool(false)]).await.expect_err("no hub");
    assert_eq!(no_hub_err.code, "MarketHubOffline");

    // Medium Astrahus cannot host Standup Market Hub
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&OWNER) { ch.structure_id = Some(ast_id); } });
    sess.structure_id = Some(ast_id);
    let ast_hub_err = rpc(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &[
        PyRep::Integer(ast_id), PyRep::Integer(34), PyRep::Real(10.0), PyRep::Integer(100),
        PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(14), PyRep::Bool(false),
    ]).await.expect_err("astrahus market");
    assert_eq!(ast_hub_err.code, "MarketHubOffline");
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&OWNER) { ch.structure_id = Some(fort_id); } });
    sess.structure_id = Some(fort_id);

    // Fit Standup Market Hub (35892) into Fortizar and deposit fuel
    let market_mod = grant(&ctx, OWNER, fort_id, 35892, InventoryFlag::ServiceSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(market_mod, true);
        if let Some(st) = snap.structures.get_mut(&fort_id) { st.fuel_blocks = 5_000; }
    });
    assert!(ctx.db.with_snapshot(|snap| structure_allows_market_orders(&ctx, snap, fort_id)));
    let sell_res = rpc(&ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder", &[sell_entry, PyRep::Bool(false)]).await.expect("PlaceMultiSellOrder");
    assert!(kv_i64(&sell_res, "orderID") > 0);

    // 2. Clone Bay Service: Jump Clone, Home Station, pod respawn inside structure
    let clone_mod = grant(&ctx, OWNER, fort_id, 35894, InventoryFlag::ServiceSlot1, 1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(clone_mod, true);
        snap.skills.entry(OWNER).or_default().insert(24242, TrainedSkillRecord { type_id: 24242, level: 5, skill_points: 256_000.0 });
    });
    let jc_res = rpc(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStructure", &[]).await.expect("InstallCloneInStructure");
    assert!(kv_i64(&jc_res, "jumpCloneID") > 0);
    assert_eq!(rpc(&ctx, &mut sess, "charMgr", "SetHomeStation", &[PyRep::Integer(fort_id)]).await.expect("SetHomeStation").as_i64(), Some(fort_id));

    rpc(&ctx, &mut sess, "ship", "LeaveShip", &[]).await.expect("LeaveShip");
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock capsule");
    rpc(&ctx, &mut sess, "ship", "SelfDestruct", &[]).await.expect("SelfDestruct");
    ctx.advance_virtual_clock(125);
    let _ = rpc(&ctx, &mut sess, "charMgr", "GetHomeStation", &[]).await;
    assert_eq!(sess.structure_id, Some(fort_id), "pod respawns inside online clone bay structure");
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "ship", "Board", &[PyRep::Integer(ship_id)]).await.expect("Board ship");

    // 3. Reprocessing Service: Compression & Structure Yield Bonus (Athanor 51%, Tatara 52.75%)
    let athanor_id = online_structure(&ctx, &mut sess, 35835, "Mining Athanor", 4.0).await;
    let tatara_id = online_structure(&ctx, &mut sess, 35836, "Mining Tatara", 5.0).await;
    let ore_id = grant(&ctx, OWNER, athanor_id, 1230, InventoryFlag::Hangar, 500);
    let ice_id = grant(&ctx, OWNER, athanor_id, 16262, InventoryFlag::Hangar, 5);

    let reproc_mod_ath = grant(&ctx, OWNER, athanor_id, 35899, InventoryFlag::ServiceSlot0, 1);
    let reproc_mod_tat = grant(&ctx, OWNER, tatara_id, 35899, InventoryFlag::ServiceSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(reproc_mod_ath, true);
        snap.module_online_states.insert(reproc_mod_tat, true);
        if let Some(s) = snap.structures.get_mut(&athanor_id) { s.fuel_blocks = 0; }
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.structure_id = Some(athanor_id); ch.station_id = None;
        }
    });
    sess.structure_id = Some(athanor_id);
    sess.station_id = None;

    let unful_comp = rpc(&ctx, &mut sess, "structureCompressionMgr", "CompressItemInStructure", &[PyRep::Integer(ore_id)]).await.expect_err("unfueled compression");
    assert_eq!(unful_comp.code, "ReprocessingServiceOffline");

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(reproc_mod_ath, true);
        snap.module_online_states.insert(reproc_mod_tat, true);
        if let Some(s) = snap.structures.get_mut(&athanor_id) { s.fuel_blocks = 1_000; }
        if let Some(s) = snap.structures.get_mut(&tatara_id) { s.fuel_blocks = 1_000; }
    });

    let comp_ore = rpc(&ctx, &mut sess, "structureCompressionMgr", "CompressItemInStructure", &[PyRep::Integer(ore_id)]).await.expect("compress ore");
    assert_eq!(comp_ore.dict_get("compressedTypeID").and_then(|v| v.as_i64()), Some(62516));
    assert_eq!(comp_ore.dict_get("compressedUnits").and_then(|v| v.as_i64()), Some(500));

    let comp_ice = rpc(&ctx, &mut sess, "structureCompressionMgr", "CompressItemInStructure", &[PyRep::Integer(ice_id)]).await.expect("compress ice");
    assert_eq!(comp_ice.dict_get("compressedTypeID").and_then(|v| v.as_i64()), Some(28434));
    assert_eq!(comp_ice.dict_get("compressedUnits").and_then(|v| v.as_i64()), Some(5));

    let ath_quotes = rpc(&ctx, &mut sess, "reprocessingSvc", "GetQuotes", &[]).await.expect("athanor quotes");
    let ath_base = ath_quotes.dict_get("stationBaseYield").and_then(|v| v.as_f64()).unwrap();
    assert!((ath_base - 0.51).abs() < 1e-6, "Athanor base yield must be 51.0%");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) { ch.structure_id = Some(tatara_id); }
    });
    sess.structure_id = Some(tatara_id);
    let tat_quotes = rpc(&ctx, &mut sess, "reprocessingSvc", "GetQuotes", &[]).await.expect("tatara quotes");
    let tat_base = tat_quotes.dict_get("stationBaseYield").and_then(|v| v.as_f64()).unwrap();
    assert!((tat_base - 0.5275).abs() < 1e-6, "Tatara base yield must be 52.75%");
}
