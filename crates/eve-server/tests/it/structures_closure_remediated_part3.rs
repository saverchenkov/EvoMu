//! End-to-end remediation tests for Upwell docking ACLs & hull docking matrix
//! (`feat:structures.docking-acl`), Repair Shop service quotes & restoration
//! (`feat:structures.repairshop-service-quotes-and-restoration`), and Mobile Cynosural Inhibitor
//! lifecycle & activation (`feat:structures.mobile-cyno-inhibitor`).

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

const OWNER: i64 = 90_000_001;
const ALLY_PILOT: i64 = 90_000_002;
const BLOCKED_PILOT: i64 = 90_000_003;
const OUTSIDER: i64 = 90_000_004;
const PERIMETER: u32 = 30_000_144;
const RANCER: u32 = 30_002_718;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 {
    rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}

fn kv_f64(rep: &PyRep, key: &str) -> f64 {
    rep.dict_get(key).and_then(|v| v.as_f64()).unwrap_or(0.0)
}

fn make_structure(
    structure_id: i64,
    type_id: u32,
    owner_corp_id: i64,
    solar_system_id: u32,
    name: &str,
    pos: DVec3,
    fuel_blocks: u32,
) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id,
        type_id,
        owner_corp_id,
        solar_system_id,
        name: name.into(),
        state: life::STATE_SHIELD_VULNERABLE.into(),
        position: pos.to_array(),
        quantum_core_installed: true,
        fuel_blocks,
        shield_hp: 1_000_000.0,
        armor_hp: 1_000_000.0,
        hull_hp: 1_000_000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

fn grant(
    ctx: &ServerContext,
    owner: i64,
    location: i64,
    type_id: u32,
    flag: InventoryFlag,
    qty: i32,
) -> i64 {
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
                ..Default::default()
            },
        );
        id
    })
}

fn new_ctx() -> Arc<ServerContext> {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

async fn setup_owner_corp(ctx: &ServerContext, sess_id: u64, sys_id: u32, name: &str, ticker: &str) -> SessionState {
    let station_id = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == sys_id)
        .map(|s| s.station_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            let ship_id = ch.active_ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = station_id;
            if let Some(sh) = snap.items.get_mut(&ship_id) {
                sh.location_id = station_id.unwrap_or(i64::from(sys_id));
            }
        }
    });
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())])
        .await
        .expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)])
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    rpc(
        ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String(name.into()), PyRep::String(ticker.into())],
    )
    .await
    .expect("AddCorporation");
    ctx.advance_virtual_clock(7 * 86_400 + 12);
    if station_id.is_some() {
        rpc(ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    }
    sess
}

async fn spawn_pilot_ship(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    corp_id: i64,
    sys_id: u32,
    ship_id: i64,
    ship_type_id: u32,
    pos: DVec3,
) {
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.advance_virtual_clock(15);
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                radius: 40.0,
                mass: 1.0e6,
                mode: MovementMode::Stop,
                speed_fraction: 0.0,
                max_velocity: 300.0,
                inertia: 1.0,
            },
            ctx.build_type_combat_state(ship_type_id),
        )
        .await;
    let _ = sys.set_ball_position(ship_id, pos).await;
}

#[tokio::test]
async fn test_remediated_docking_acl_and_upwell_hull_matrix() {
    // parity: feat:structures.docking-acl
    eve_testkit::attest(&["feat:structures.docking-acl"]);

    let ctx = new_ctx();
    let mut sess = setup_owner_corp(&ctx, 8201, PERIMETER, "Docking Matrix Corp", "DOCK").await;
    let corp_id = sess.corporation_id.expect("corp");
    let st_pos = DVec3::new(0.0, 5.0e9, 0.0);

    // 1. Live ACL enforcement on stationSvc.Dock & structureDocking.Dock via ownerGroupManager + structureSettings.
    ctx.db.with_mut_snapshot(|snap| {
        for (cid, corp) in [(ALLY_PILOT, 98_820_002), (BLOCKED_PILOT, 98_820_003), (OUTSIDER, 98_820_004)] {
            let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
            ch.character_id = cid;
            ch.corporation_id = corp;
            snap.characters.insert(cid, ch);
        }
    });
    let group_id = kv_i64(
        &rpc(&ctx, &mut sess, "ownerGroupManager", "Create", &[PyRep::String("Docking ACL".into()), PyRep::String("Allowed".into())])
            .await
            .expect("Create ACL group"),
        "groupID",
    );
    rpc(&ctx, &mut sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(ALLY_PILOT), PyRep::Integer(BLOCKED_PILOT)])])
        .await
        .expect("AddMembers");
    rpc(&ctx, &mut sess, "ownerGroupManager", "UpdateMembershipTypes", &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(BLOCKED_PILOT)]), PyRep::Integer(-1)])
        .await
        .expect("Block pilot in ACL group");

    let prof_id = rpc(&ctx, &mut sess, "structureProfiles", "CreateProfile", &[PyRep::String("ACL Profile".into()), PyRep::String("Docking ACL".into())])
        .await
        .expect("CreateProfile")
        .as_i64()
        .unwrap();
    rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(prof_id),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(0), PyRep::Integer(1)]),
                PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(group_id), PyRep::Integer(1)]),
                PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(BLOCKED_PILOT), PyRep::Integer(-1)]),
            ]),
        ],
    )
    .await
    .expect("SaveProfileSettings");

    let astra_id = 880_820_101;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            astra_id,
            make_structure(astra_id, 35832, corp_id, PERIMETER, "ACL Astrahus", st_pos, 1_000),
        );
    });
    life::sync_all_structures(&ctx);
    rpc(&ctx, &mut sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(astra_id)]), PyRep::Integer(prof_id)])
        .await
        .expect("ChangeProfiles");

    let mut ally_sess = SessionState::new(8202);
    ally_sess.user_id = Some(2);
    spawn_pilot_ship(&ctx, &mut ally_sess, ALLY_PILOT, 98_820_002, PERIMETER, 880_820_111, 587, st_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut ally_sess, "stationSvc", "Dock", &[PyRep::Integer(astra_id)])
        .await
        .expect("Allowed ACL pilot docks via stationSvc.Dock");
    assert_eq!(ally_sess.structure_id, Some(astra_id));

    let mut blocked_sess = SessionState::new(8203);
    blocked_sess.user_id = Some(3);
    spawn_pilot_ship(&ctx, &mut blocked_sess, BLOCKED_PILOT, 98_820_003, PERIMETER, 880_820_112, 587, st_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    assert_eq!(
        rpc(&ctx, &mut blocked_sess, "structureDocking", "Dock", &[PyRep::Integer(astra_id)])
            .await
            .expect_err("Blocked ACL pilot denied")
            .code,
        "StructureDockingAccessDenied"
    );

    let mut outsider_sess = SessionState::new(8204);
    outsider_sess.user_id = Some(4);
    spawn_pilot_ship(&ctx, &mut outsider_sess, OUTSIDER, 98_820_004, PERIMETER, 880_820_113, 587, st_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    assert_eq!(
        rpc(&ctx, &mut outsider_sess, "structureDocking", "Dock", &[PyRep::Integer(astra_id)])
            .await
            .expect_err("Unaffiliated pilot denied")
            .code,
        "StructureDockingAccessDenied"
    );

    // 2. Full V24 Upwell Hull Docking Matrix across non-dockable, Medium, Tatara, Large, and XLarge structures.
    let hulls: &[(i64, u32, &[u32], &[u32])] = &[
        (880_820_201, 35840, &[], &[587]),
        (880_820_202, 35841, &[], &[587]),
        (880_820_203, 37534, &[], &[587]),
        (880_820_204, 81826, &[], &[587]),
        (880_820_205, 35832, &[587], &[28352, 19720]),
        (880_820_206, 35825, &[587], &[28352, 19720]),
        (880_820_207, 35835, &[587], &[28352, 19720]),
        (880_820_208, 35826, &[587], &[28352, 19720]),
        (880_820_209, 35836, &[587, 28352], &[19720, 23919]),
        (880_820_210, 35833, &[587, 28352, 19720], &[23919, 671]),
        (880_820_211, 47512, &[587, 28352, 19720], &[23919, 671]),
        (880_820_212, 35827, &[587, 28352, 19720], &[23919, 671]),
        (880_820_213, 35834, &[587, 28352, 19720, 23919, 671], &[]),
        (880_820_214, 40340, &[587, 28352, 19720, 23919, 671], &[]),
    ];

    for (idx, &(sid, stype, allowed_ships, denied_ships)) in hulls.iter().enumerate() {
        let pos = st_pos + DVec3::new((idx as f64 + 1.0) * 5_000_000.0, 0.0, 0.0);
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(
                sid,
                make_structure(sid, stype, corp_id, PERIMETER, &format!("Matrix Structure {stype}"), pos, 1_000),
            );
        });
        life::sync_all_structures(&ctx);

        for &ship_tid in denied_ships {
            let test_ship = 880_820_500 + (idx as i64) * 20 + i64::from(ship_tid % 17);
            spawn_pilot_ship(&ctx, &mut sess, OWNER, corp_id, PERIMETER, test_ship, ship_tid, pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
            let err = rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(sid)])
                .await
                .expect_err("denied hull docking");
            let expected_code = if allowed_ships.is_empty() {
                "StructureDoesNotAllowDocking"
            } else {
                "ShipTooLargeForStructure"
            };
            assert_eq!(err.code, expected_code, "structure {stype} vs ship {ship_tid}");
        }

        for &ship_tid in allowed_ships {
            let test_ship = 880_820_800 + (idx as i64) * 20 + i64::from(ship_tid % 19);
            spawn_pilot_ship(&ctx, &mut sess, OWNER, corp_id, PERIMETER, test_ship, ship_tid, pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
            rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(sid)])
                .await
                .unwrap_or_else(|e| panic!("structure {stype} must allow ship {ship_tid}: {e:?}"));
            assert_eq!(sess.structure_id, Some(sid));
        }
    }
}

#[tokio::test]
async fn test_remediated_repairshop_service_quotes_and_restoration() {
    // parity: feat:structures.repairshop-service-quotes-and-restoration
    eve_testkit::attest(&["feat:structures.repairshop-service-quotes-and-restoration"]);

    let ctx = new_ctx();
    let mut sess = setup_owner_corp(&ctx, 8210, PERIMETER, "Repair Grid Corp", "REPR").await;
    let corp_id = sess.corporation_id.expect("corp");
    let st_id = 880_821_001;
    let st_pos = DVec3::new(0.0, 5.0e9, 0.0);
    let ship_id = 880_821_010;
    let mod_id = 880_821_011;
    let drone_id = 880_821_012;

    // Seed an unfueled (LowPower) Astrahus WITH Quantum Core installed, plus 5% Repair Shop tax (500 bps).
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            st_id,
            make_structure(st_id, 35832, corp_id, PERIMETER, "LowPower Repair Astrahus", st_pos, 0),
        );
        let mut ext = load_ext_state(snap);
        ext.access_profiles.entry(st_id).or_default().service_settings.insert(13, 500);
        save_ext_state(snap, &ext);

        snap.characters.get_mut(&OWNER).unwrap().balance = eve_db::IskCents::from_f64_isk(10_000_000.0);
        snap.corporations.get_mut(&corp_id).unwrap().set_division_balance(1, eve_db::IskCents::from_f64_isk(500_000.0));
        let ch = snap.characters.get_mut(&OWNER).unwrap();
        ch.station_id = None;
        ch.structure_id = Some(st_id);
        ch.solar_system_id = PERIMETER;
        ch.active_ship_id = ship_id;

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter (category 6)
                owner_id: OWNER,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([
                    (attr_ids::DAMAGE, 40.0),
                    (attr_ids::ARMOR_DAMAGE, 60.0),
                ]),
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 3170, // Small Armor Repairer I (category 7)
                owner_id: OWNER,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(attr_ids::DAMAGE, 20.0)]),
                ..Default::default()
            },
        );
        snap.items.insert(
            drone_id,
            InventoryItemRecord {
                item_id: drone_id,
                type_id: 2454, // Hobgoblin I (category 18)
                owner_id: OWNER,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([
                    (attr_ids::DAMAGE, 10.0),
                    (attr_ids::ARMOR_DAMAGE, 15.0),
                ]),
                ..Default::default()
            },
        );
    });
    sess.station_id = None;
    sess.structure_id = Some(st_id);
    sess.solar_system_id = Some(PERIMETER);
    sess.ship_id = Some(ship_id);

    // 1. Verify LowPower structure WITH Quantum Core provides Repair Shop quotes using exact evemu_Crucible modifiers!
    let quotes = rpc(&ctx, &mut sess, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![PyRep::Integer(ship_id)])])
        .await
        .expect("LowPower structure with Quantum Core must offer Repair Shop quotes");
    let PyRep::Dict(q_dict) = quotes else { panic!("expected dict") };
    let (_, PyRep::List(rows)) = &q_dict[0] else { panic!("expected quote list") };
    assert_eq!(rows.len(), 3, "Ship + fitted module + drone in bay must all be quoted");

    let ship_base = ctx.sde.types.get(&587).map(|t| t.base_price).filter(|p| *p > 0.0).unwrap_or(10_000.0);
    let mod_base = ctx.sde.types.get(&3170).map(|t| t.base_price).filter(|p| *p > 0.0).unwrap_or(10_000.0);
    let ship_unit_cents = (eve_db::IskCents::from_f64_isk((ship_base * 7.5e-6).max(0.01)).0 * 500) / 10_000;
    let mod_unit_cents = (eve_db::IskCents::from_f64_isk((mod_base * 1.25e-3).max(0.01)).0 * 500) / 10_000;
    let expected_ship_tax = (ship_unit_cents * 100) as f64 / 100.0;
    let expected_mod_tax = (mod_unit_cents * 20) as f64 / 100.0;

    let ship_row = rows.iter().find(|r| kv_i64(r, "itemID") == ship_id).expect("ship quote");
    let mod_row = rows.iter().find(|r| kv_i64(r, "itemID") == mod_id).expect("mod quote");
    assert_eq!(kv_f64(ship_row, "baseCost"), 0.0);
    assert!((kv_f64(ship_row, "totalRepairCost") - expected_ship_tax).abs() < 1e-2);
    assert!((kv_f64(mod_row, "totalRepairCost") - expected_mod_tax).abs() < 1e-2);

    // 2. Verify NoCore and Abandoned structures reject Repair Shop, then restore all HP in FullPower/LowPower.
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&st_id).unwrap().quantum_core_installed = false);
    assert_eq!(
        rpc(&ctx, &mut sess, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![PyRep::Integer(ship_id)])])
            .await
            .expect_err("NoCore quote rejected")
            .code,
        "QuantumCoreRequired"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&st_id).unwrap().quantum_core_installed = true;
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(st_id).or_default().low_power_since_unix = Some(1);
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(ship_id)])])
            .await
            .expect_err("Abandoned repair rejected")
            .code,
        "StructureAbandoned"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(st_id).or_default().low_power_since_unix = None;
        save_ext_state(snap, &ext);
    });

    let total_expected_isk: f64 = rows.iter().map(|r| kv_f64(r, "totalRepairCost")).sum();
    let rep_res = rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(ship_id)])])
        .await
        .expect("RepairItemsInStructure succeeds in LowPower with Quantum Core");
    let paid = kv_f64(&rep_res, "totalCostISK");
    assert!((paid - total_expected_isk).abs() < 1e-2);

    ctx.db.with_snapshot(|snap| {
        for iid in [ship_id, mod_id, drone_id] {
            let it = snap.items.get(&iid).unwrap();
            assert!(!it.dynamic_attributes.contains_key(&attr_ids::DAMAGE));
            assert!(!it.dynamic_attributes.contains_key(&attr_ids::ARMOR_DAMAGE));
        }
        assert!((snap.characters.get(&OWNER).unwrap().balance.to_wire_f64() - (10_000_000.0 - paid)).abs() < 1e-2);
        assert!((snap.corporations.get(&corp_id).unwrap().division_balance(1).to_wire_f64() - (500_000.0 + paid)).abs() < 1e-2);
    });
}

#[tokio::test]
async fn test_remediated_mobile_cyno_inhibitor_lifecycle_and_activation() {
    // parity: feat:structures.mobile-cyno-inhibitor
    eve_testkit::attest(&["feat:structures.mobile-cyno-inhibitor"]);

    let ctx = new_ctx();
    let mut sess = setup_owner_corp(&ctx, 8220, PERIMETER, "Cyno Inhibitor Corp", "CINH").await;
    let corp_id = sess.corporation_id.expect("corp");
    let base_pos = DVec3::new(0.0, 5.0e9, 0.0);
    let falcon_id = 880_822_010;
    let venture_id = 880_822_011;

    spawn_pilot_ship(&ctx, &mut sess, OWNER, corp_id, RANCER, falcon_id, 11957, base_pos).await;
    let inhib_1 = grant(&ctx, OWNER, falcon_id, 33476, InventoryFlag::Cargo, 1);
    let inhib_2 = grant(&ctx, OWNER, falcon_id, 33476, InventoryFlag::Cargo, 1);
    let std_cyno = grant(&ctx, OWNER, falcon_id, 21096, InventoryFlag::HiSlot0, 1);
    let cov_cyno = grant(&ctx, OWNER, falcon_id, 28646, InventoryFlag::HiSlot1, 1);
    let _ozone = grant(&ctx, OWNER, falcon_id, 16273, InventoryFlag::Cargo, 5_000);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(std_cyno, true);
        snap.module_online_states.insert(cov_cyno, true);
        let sk = snap.skills.entry(OWNER).or_default();
        sk.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 5, skill_points: 256_000.0 });
        sk.insert(21603, eve_db::TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
        sk.insert(21611, eve_db::TrainedSkillRecord { type_id: 21611, level: 5, skill_points: 256_000.0 });
        snap.fleets.insert(5001, eve_db::FleetRecord { fleet_id: 5001, boss_id: OWNER, members: vec![OWNER], ..Default::default() });
    });
    ctx.invalidate_ship_fitting(falcon_id);

    // 1. Deploy Mobile Cynosural Inhibitor via posMgr.AnchorStructure -> 120s delay, 3,600s lifetime, non-scoopable, 200 km overlap ban.
    let t0 = ctx.now_unix();
    let anc1 = rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(inhib_1)])
        .await
        .expect("Anchor Mobile Cynosural Inhibitor");
    assert_eq!(kv_f64(&anc1, "anchoringDelaySeconds"), 120.0);
    assert_eq!(kv_i64(&anc1, "despawnUnix"), (t0 + 3_600) as i64);
    assert!(
        !eve_server::services::mobile_deployables::can_deployable_be_scooped(1249),
        "Mobile Cynosural Inhibitor (group 1249) must not be scoopable"
    );

    // Second Mobile Cyno Inhibitor within 200 km is rejected with MobileCynoInhibitorOverlap.
    assert_eq!(
        rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(inhib_2)])
            .await
            .expect_err("200 km overlap rejected")
            .code,
        "MobileCynoInhibitorOverlap"
    );

    let inhib_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.item_positions[&inhib_1]));
    let sys = ctx.systems.get_or_spawn(RANCER);
    let _ = sys.set_ball_position(falcon_id, inhib_pos + DVec3::new(50_000.0, 0.0, 0.0)).await;

    // 2. During initial 120s spool-up (t + 60s), inhibitor is not yet online -> standard cyno succeeds.
    ctx.advance_virtual_clock(60);
    rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(std_cyno), PyRep::String("cynosuralGeneration".into())])
        .await
        .expect("Standard cyno succeeds during 120s activation delay");

    // 3. After 120s (t + 125s): Standard & Industrial cynos blocked within 100 km, Covert cyno allowed, > 100 km allowed!
    ctx.advance_virtual_clock(65);
    assert_eq!(
        rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(std_cyno), PyRep::String("cynosuralGeneration".into())])
            .await
            .expect_err("Standard cyno blocked within 100 km")
            .code,
        "CynoInhibited"
    );

    // Covert Cynosural Field Generator I (28646) is unaffected by Mobile Cynosural Inhibitor!
    let covops_id = falcon_id + 10;
    spawn_pilot_ship(&ctx, &mut sess, OWNER, corp_id, RANCER, covops_id, 11_188, inhib_pos + DVec3::new(50_000.0, 0.0, 0.0)).await;
    let cov_cyno_2 = grant(&ctx, OWNER, covops_id, 28646, InventoryFlag::HiSlot0, 1);
    let _ozone_cov = grant(&ctx, OWNER, covops_id, 16273, InventoryFlag::Cargo, 500);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(cov_cyno_2, true);
    });
    ctx.invalidate_ship_fitting(covops_id);
    rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(cov_cyno_2), PyRep::String("cynosuralGeneration".into())])
        .await
        .expect("Covert cyno succeeds inside 100 km of Mobile Cyno Inhibitor");

    // Industrial Cynosural Field Generator (52694) on a Venture (32880) at 50 km is also blocked!
    spawn_pilot_ship(&ctx, &mut sess, OWNER, corp_id, RANCER, venture_id, 32880, inhib_pos + DVec3::new(50_000.0, 0.0, 0.0)).await;
    let indy_cyno = grant(&ctx, OWNER, venture_id, 52694, InventoryFlag::HiSlot0, 1);
    let _ozone_v = grant(&ctx, OWNER, venture_id, 16273, InventoryFlag::Cargo, 5_000);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(indy_cyno, true);
    });
    ctx.invalidate_ship_fitting(venture_id);
    assert_eq!(
        rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(indy_cyno), PyRep::String("cynosuralGeneration".into())])
            .await
            .expect_err("Industrial cyno blocked within 100 km")
            .code,
        "CynoInhibited"
    );

    // Move Falcon to 150 km (> 100 km) after prior cyno expires -> Standard cyno succeeds!
    ctx.advance_virtual_clock(600);
    spawn_pilot_ship(&ctx, &mut sess, OWNER, corp_id, RANCER, falcon_id, 11957, inhib_pos + DVec3::new(150_000.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(std_cyno), PyRep::String("cynosuralGeneration".into())])
        .await
        .expect("Standard cyno succeeds beyond 100 km");

    // 4. After 1-hour (3,600s) lifetime, inhibitor expires and standard cyno at 50 km succeeds again!
    ctx.advance_virtual_clock(3_500);
    let _ = sys.set_ball_position(falcon_id, inhib_pos + DVec3::new(50_000.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(std_cyno), PyRep::String("cynosuralGeneration".into())])
        .await
        .expect("Standard cyno succeeds after 1-hour inhibitor expiry");
}
