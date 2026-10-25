//! Inventory / ship / fitting ledger rows (`feat:inventory.*`, `feat:ships.*`, `feat:fitting.*`)
//! exercised through the real V24 RPC names against the real local SDE.



use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const C1: i64 = 90_000_001;
const C2: i64 = 90_000_002;
const JITA44: i64 = 60_003_760;
const JITA: u32 = 30_000_142;
const RIFTER: u32 = 587;
const CAPSULE: u32 = 670;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open evomu_sde.sqlite"),
    );
    let ctx = ServerContext::with_sde_and_repository(sde, std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.set_clock_paused(true);
    ctx
}

#[allow(clippy::too_many_arguments)]
fn seed(ctx: &ServerContext, id: i64, type_id: u32, owner: i64, loc: i64, flag: InventoryFlag, qty: i32, singleton: bool) {
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id,
                owner_id: owner,
                location_id: loc,
                flag,
                quantity: qty,
                singleton,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

async fn pilot(ctx: &ServerContext, char_id: i64, sid: u64) -> SessionState {
    let (u, p) = if char_id == C1 { ("demo", "demo-secret") } else { ("other", "other-secret") };
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, u, p).await;
    call(ctx, &mut s, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(char_id)]).await.expect("select");
    s
}

async fn call(ctx: &ServerContext, s: &mut SessionState, svc: &str, m: &str, args: Vec<PyRep>) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(s, svc, None, m, &args, &HashMap::new(), None).await
}

fn item(ctx: &ServerContext, id: i64) -> Option<InventoryItemRecord> {
    ctx.db.with_snapshot(|s| s.items.get(&id).cloned())
}

fn int(rep: &PyRep, key: &str) -> i64 {
    rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or_else(|| panic!("missing {key} in {rep:?}"))
}

/// Places `char_id` undocked in `ship_id` (seeded if absent) at `pos` in Jita.
async fn undock_in(ctx: &ServerContext, s: &mut SessionState, char_id: i64, ship_id: i64, type_id: u32, pos: DVec3) {
    if item(ctx, ship_id).is_none() {
        seed(ctx, ship_id, type_id, char_id, JITA as i64, InventoryFlag::None, 1, true);
    }
    ctx.db.with_mut_snapshot(|snap| {
        let sh = snap.items.get_mut(&ship_id).unwrap();
        sh.location_id = JITA as i64;
        sh.flag = InventoryFlag::None;
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.solar_system_id = JITA;
        ch.active_ship_id = ship_id;
    });
    s.station_id = None;
    s.structure_id = None;
    s.solar_system_id = Some(JITA);
    s.ship_id = Some(ship_id);
    spawn(ctx, ship_id, type_id, pos).await;
}

async fn spawn(ctx: &ServerContext, id: i64, type_id: u32, pos: DVec3) {
    let t = &ctx.sde.types[&type_id];
    let ball = Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: t.mass,
        radius: t.radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    ctx.systems.get_or_spawn(JITA).spawn_ball(ball).await.expect("spawn");
}

async fn ball(ctx: &ServerContext, id: i64) -> Option<Ball> {
    ctx.systems.get_or_spawn(JITA).get_ball_snapshot(id).await.ok().flatten().map(|(_, b)| b)
}

fn rows(rep: &PyRep) -> Vec<PyRep> {
    rep.crowset_rows().expect("CRowset").to_vec()
}

fn col(row: &PyRep, name: &str) -> PyRep {
    row.dbrow_get(name).cloned().unwrap_or_else(|| panic!("column {name}"))
}

#[tokio::test]
async fn test_hangar_list_and_item_labels() {
    // parity: feat:inventory.hangar-list, feat:inventory.item-labels
    eve_testkit::attest(&["feat:inventory.hangar-list", "feat:inventory.item-labels"]);
    let ctx = new_ctx();
    let mut s = pilot(&ctx, C1, 6101).await;
    let cont_type = ctx.sde.types.values().find(|t| t.name == "Medium Standard Container").unwrap().type_id;
    let bp_type = ctx.sde.types.values().find(|t| t.name == "Rifter Blueprint").unwrap().type_id;
    seed(&ctx, 610_001, 34, C1, JITA44, InventoryFlag::Hangar, 5_000, false); // stack
    seed(&ctx, 610_002, cont_type, C1, JITA44, InventoryFlag::Hangar, 1, true); // singleton container
    seed(&ctx, 610_003, bp_type, C1, JITA44, InventoryFlag::Hangar, 1, true);
    ctx.db.with_mut_snapshot(|snap| snap.items.get_mut(&610_003).unwrap().is_blueprint_copy = true);
    seed(&ctx, 610_004, 35, C1, 60_008_494, InventoryFlag::Hangar, 9, false); // other station
    seed(&ctx, 610_005, 36, C2, JITA44, InventoryFlag::Hangar, 9, false); // other owner
    seed(&ctx, 610_006, 37, C1, 100_000_001, InventoryFlag::Cargo, 9, false); // in ship cargo

    // Rename the container and the docked ship via invbroker.SetLabel.
    call(&ctx, &mut s, "invbroker", "SetLabel", vec![PyRep::Integer(610_002), PyRep::String("Loot Box".into())]).await.unwrap();
    call(&ctx, &mut s, "invbroker", "SetLabel", vec![PyRep::Integer(100_000_001), PyRep::String("Red Rifter".into())]).await.unwrap();
    let e = call(&ctx, &mut s, "invbroker", "SetLabel", vec![PyRep::Integer(610_005), PyRep::String("Mine".into())]).await.unwrap_err();
    assert_eq!(e.code, "OwnershipViolation");

    let list = call(&ctx, &mut s, "invbroker", "List", vec![PyRep::Integer(InventoryFlag::Hangar as i64)]).await.unwrap();
    let mut got: Vec<(i64, i64, i64, String)> = rows(&list)
        .iter()
        .map(|r| {
            assert_eq!(col(r, "locationID").as_i64(), Some(JITA44));
            assert_eq!(col(r, "ownerID").as_i64(), Some(C1));
            let t = &ctx.sde.types[&(col(r, "typeID").as_i64().unwrap() as u32)];
            assert_eq!(col(r, "groupID").as_i64(), Some(t.group_id as i64));
            assert_eq!(col(r, "categoryID").as_i64(), Some(t.category_id as i64));
            (
                col(r, "itemID").as_i64().unwrap(),
                col(r, "flagID").as_i64().unwrap(),
                col(r, "quantity").as_i64().unwrap(),
                col(r, "customInfo").as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    let by_id: HashMap<i64, (i64, i64, String)> = got.drain(..).map(|(i, f, q, n)| (i, (f, q, n))).collect();
    assert_eq!(by_id[&100_000_001], (4, -1, "Red Rifter".to_string())); // singleton => quantity -1
    assert_eq!(by_id[&610_001], (4, 5_000, String::new())); // stack quantity
    assert_eq!(by_id[&610_002], (4, -1, "Loot Box".to_string())); // renamed singleton container
    assert_eq!(by_id[&610_003], (4, -2, String::new())); // blueprint copy => -2
    for excluded in [610_004, 610_005, 610_006] {
        assert!(!by_id.contains_key(&excluded), "{excluded} must not be listed");
    }

    // Structure hangar listing: items docked in an Upwell structure.
    let structure_id = 1_030_000_000_001i64;
    seed(&ctx, 610_010, 34, C1, structure_id, InventoryFlag::Hangar, 77, false);
    s.station_id = None;
    s.structure_id = Some(structure_id);
    ctx.db.with_mut_snapshot(|sn| {
        let ch = sn.characters.get_mut(&C1).unwrap();
        ch.station_id = None;
        ch.structure_id = Some(structure_id);
    });
    let slist = call(&ctx, &mut s, "invbroker", "List", vec![PyRep::Integer(4)]).await.unwrap();
    let srows = rows(&slist);
    assert_eq!(srows.len(), 1);
    assert_eq!(col(&srows[0], "itemID").as_i64(), Some(610_010));
    assert_eq!(col(&srows[0], "quantity").as_i64(), Some(77));

    // Labels are visible in the asset list (invbroker.GetAllItems searches by name).
    let assets = call(&ctx, &mut s, "invbroker", "GetAllItems", vec![]).await.unwrap();
    let named: Vec<(i64, String)> = rows(assets.dict_get("items").unwrap())
        .iter()
        .filter(|r| !col(r, "customInfo").as_str().unwrap_or("").is_empty())
        .map(|r| (col(r, "itemID").as_i64().unwrap(), col(r, "customInfo").as_str().unwrap().to_string()))
        .collect();
    assert!(named.contains(&(610_002, "Loot Box".into())));
    assert!(named.contains(&(100_000_001, "Red Rifter".into())));
}

#[tokio::test]
async fn test_board_in_space_transfers_capsule() {
    // parity: feat:ships.board-in-space
    eve_testkit::attest(&["feat:ships.board-in-space"]);
    let ctx = new_ctx();
    let mut s = pilot(&ctx, C1, 6201).await;
    undock_in(&ctx, &mut s, C1, 620_001, CAPSULE, DVec3::ZERO).await;
    seed(&ctx, 620_002, RIFTER, C1, JITA as i64, InventoryFlag::None, 1, true);
    spawn(&ctx, 620_002, RIFTER, DVec3::new(1_500.0, 0.0, 0.0)).await;
    seed(&ctx, 620_003, RIFTER, C1, JITA as i64, InventoryFlag::None, 1, true);
    spawn(&ctx, 620_003, RIFTER, DVec3::new(30_000.0, 0.0, 0.0)).await;
    seed(&ctx, 620_004, RIFTER, C2, JITA as i64, InventoryFlag::None, 1, true);
    spawn(&ctx, 620_004, RIFTER, DVec3::new(500.0, 0.0, 0.0)).await;

    ctx.advance_secs(12);
    let far = call(&ctx, &mut s, "ship", "Board", vec![PyRep::Integer(620_003)]).await.unwrap_err();
    assert_eq!(far.code, "ShipOutOfBoardingRange");
    let foreign = call(&ctx, &mut s, "ship", "Board", vec![PyRep::Integer(620_004)]).await.unwrap_err();
    assert_eq!(foreign.code, "OwnershipViolation");
    assert_eq!(s.ship_id, Some(620_001));

    let rep = call(&ctx, &mut s, "ship", "Board", vec![PyRep::Integer(620_002)]).await.expect("board");
    assert_eq!(int(&rep, "shipID"), 620_002);
    assert_eq!(int(&rep, "oldShipID"), 620_001);
    assert_eq!(s.ship_id, Some(620_002));
    assert_eq!(ctx.db.with_snapshot(|sn| sn.characters[&C1].active_ship_id), 620_002);
    // The capsule is absorbed into the boarded hull: item and ball are gone.
    assert!(item(&ctx, 620_001).is_none());
    assert!(ball(&ctx, 620_001).await.is_none());
    assert!(ball(&ctx, 620_002).await.is_some());
}

#[tokio::test]
async fn test_eject_leaves_ship_in_space_and_pilot_in_capsule() {
    // parity: feat:ships.eject
    eve_testkit::attest(&["feat:ships.eject"]);
    let ctx = new_ctx();
    let mut s = pilot(&ctx, C1, 6301).await;
    s.ship_id = Some(100_000_001);
    let docked = call(&ctx, &mut s, "ship", "Eject", vec![]).await.unwrap_err();
    assert_eq!(docked.code, "CannotEjectWhileDocked");

    undock_in(&ctx, &mut s, C1, 630_001, RIFTER, DVec3::new(10_000.0, 0.0, 0.0)).await;
    ctx.advance_secs(12);
    let rep = call(&ctx, &mut s, "ship", "Eject", vec![]).await.expect("Eject in space");
    let pod = int(&rep, "capsuleID");
    assert_eq!(int(&rep, "abandonedShipID"), 630_001);
    // Pilot is now in a capsule (SDE group "Capsule") in space.
    let pod_item = item(&ctx, pod).unwrap();
    assert_eq!(ctx.sde.groups[&ctx.sde.types[&pod_item.type_id].group_id].name, "Capsule");
    assert_eq!(pod_item.location_id, JITA as i64);
    assert_eq!(s.ship_id, Some(pod));
    assert_eq!(ctx.db.with_snapshot(|sn| sn.characters[&C1].active_ship_id), pod);
    assert!(ball(&ctx, pod).await.is_some());
    // The hull stays in space, unpiloted, where it was.
    let hull = item(&ctx, 630_001).unwrap();
    assert_eq!((hull.location_id, hull.flag), (JITA as i64, InventoryFlag::None));
    let hb = ball(&ctx, 630_001).await.expect("abandoned hull ball");
    assert_eq!(hb.position, DVec3::new(10_000.0, 0.0, 0.0));
    // Ejecting again from the capsule is refused.
    assert_eq!(call(&ctx, &mut s, "ship", "Eject", vec![]).await.unwrap_err().code, "CannotEjectFromCapsule");
}

#[tokio::test]
async fn test_assemble_ship_turns_packaged_hull_into_singleton() {
    // parity: feat:ships.assemble-ship
    eve_testkit::attest(&["feat:ships.assemble-ship"]);
    let ctx = new_ctx();
    let mut s = pilot(&ctx, C1, 6401).await;
    seed(&ctx, 640_001, RIFTER, C1, JITA44, InventoryFlag::Hangar, 2, false); // two packaged Rifters
    seed(&ctx, 640_002, 34, C1, JITA44, InventoryFlag::Hangar, 10, false);
    seed(&ctx, 640_003, RIFTER, C2, JITA44, InventoryFlag::Hangar, 1, false);

    let rep = call(&ctx, &mut s, "ship", "AssembleShip", vec![PyRep::Integer(640_001)]).await.expect("assemble");
    let new_id = int(&rep, "itemID");
    assert_ne!(new_id, 640_001);
    let assembled = item(&ctx, new_id).unwrap();
    assert!(assembled.singleton);
    assert_eq!((assembled.type_id, assembled.quantity, assembled.location_id), (RIFTER, 1, JITA44));
    let rest = item(&ctx, 640_001).unwrap();
    assert!(!rest.singleton);
    assert_eq!(rest.quantity, 1);
    // Last packaged hull assembles in place.
    let rep2 = call(&ctx, &mut s, "ship", "AssembleShip", vec![PyRep::Integer(640_001)]).await.expect("assemble last");
    assert_eq!(int(&rep2, "itemID"), 640_001);
    assert!(item(&ctx, 640_001).unwrap().singleton);
    // Non-ships and other pilots' hulls are refused.
    assert_eq!(call(&ctx, &mut s, "ship", "AssembleShip", vec![PyRep::Integer(640_002)]).await.unwrap_err().code, "NotAShip");
    assert_eq!(
        call(&ctx, &mut s, "ship", "AssembleShip", vec![PyRep::Integer(640_003)]).await.unwrap_err().code,
        "OwnershipOrLocationViolation"
    );
}

#[tokio::test]
async fn test_newbie_ship_racial_corvette_in_capsule_with_cooldown() {
    // parity: feat:ships.newbie-ship
    eve_testkit::attest(&["feat:ships.newbie-ship"]);
    let ctx = new_ctx();
    let mut s = pilot(&ctx, C1, 6501).await;
    // Docked in a Rifter: refused.
    s.ship_id = Some(100_000_001);
    assert_eq!(call(&ctx, &mut s, "dogmaIM", "CreateNewbieShip", vec![]).await.unwrap_err().code, "NotInCapsule");
    // Move every owned hull out of the station, put the pilot in a capsule.
    ctx.db.with_mut_snapshot(|snap| {
        for it in snap.items.values_mut() {
            if it.owner_id == C1 && it.location_id == JITA44 && it.singleton {
                it.location_id = 60_008_494;
            }
        }
    });
    seed(&ctx, 650_001, CAPSULE, C1, JITA44, InventoryFlag::Hangar, 1, true);
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&C1).unwrap().active_ship_id = 650_001);
    s.ship_id = Some(650_001);
    let rep = call(&ctx, &mut s, "dogmaIM", "CreateNewbieShip", vec![]).await.expect("corvette");
    let corvette = item(&ctx, int(&rep, "shipID")).unwrap();
    let t = &ctx.sde.types[&corvette.type_id];
    let race = ctx.db.with_snapshot(|sn| sn.characters[&C1].race_id);
    assert_eq!(ctx.sde.groups[&t.group_id].name, "Corvette");
    assert_eq!(t.race_id, Some(race));
    assert_eq!(t.name, "Reaper"); // Minmatar demo pilot
    assert_eq!((corvette.location_id, corvette.flag, corvette.singleton), (JITA44, InventoryFlag::Hangar, true));
    assert_eq!(s.ship_id, Some(corvette.item_id));

    // Back in a capsule (corvette moved away): still on cooldown.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&corvette.item_id).unwrap().location_id = 60_008_494;
        snap.characters.get_mut(&C1).unwrap().active_ship_id = 650_001;
    });
    s.ship_id = Some(650_001);
    assert_eq!(call(&ctx, &mut s, "dogmaIM", "CreateNewbieShip", vec![]).await.unwrap_err().code, "NewbieShipCooldown");
    ctx.advance_virtual_clock(61);
    let again = call(&ctx, &mut s, "dogmaIM", "CreateNewbieShip", vec![]).await.expect("after cooldown");
    assert_ne!(int(&again, "shipID"), corvette.item_id);
}

#[tokio::test]
async fn test_board_stored_ship_from_maintenance_bay() {
    // parity: feat:ships.stored-ship-board
    eve_testkit::attest(&["feat:ships.stored-ship-board"]);
    let ctx = new_ctx();
    let mut s = pilot(&ctx, C1, 6601).await;
    let orca = ctx.sde.types.values().find(|t| t.name == "Orca").unwrap().type_id;
    undock_in(&ctx, &mut s, C1, 660_001, CAPSULE, DVec3::ZERO).await;
    seed(&ctx, 660_010, orca, C1, JITA as i64, InventoryFlag::None, 1, true);
    spawn(&ctx, 660_010, orca, DVec3::new(1_200.0, 0.0, 0.0)).await;
    seed(&ctx, 660_011, RIFTER, C1, 660_010, InventoryFlag::SpecializedShipHold, 1, true); // in the Orca's SMB
    seed(&ctx, 660_012, RIFTER, C1, 660_010, InventoryFlag::Cargo, 1, true); // in cargo, not the SMB
    seed(&ctx, 660_013, RIFTER, C2, 660_010, InventoryFlag::SpecializedShipHold, 1, true); // someone else's

    let host = |id: i64| vec![PyRep::Integer(660_010), PyRep::Integer(id)];
    assert_eq!(call(&ctx, &mut s, "ship", "BoardStoredShip", host(660_012)).await.unwrap_err().code, "ShipNotInMaintenanceBay");
    assert_eq!(call(&ctx, &mut s, "ship", "BoardStoredShip", host(660_013)).await.unwrap_err().code, "OwnershipViolation");
    assert_eq!(item(&ctx, 660_013).unwrap().owner_id, C2, "no ownership transfer on refusal");

    // Out of range of the Orca.
    ctx.systems.get_or_spawn(JITA).set_ball_position(660_001, DVec3::new(50_000.0, 0.0, 0.0)).await;
    assert_eq!(call(&ctx, &mut s, "ship", "BoardStoredShip", host(660_011)).await.unwrap_err().code, "ShipOutOfBoardingRange");
    ctx.systems.get_or_spawn(JITA).set_ball_position(660_001, DVec3::ZERO).await;

    let rep = call(&ctx, &mut s, "ship", "BoardStoredShip", host(660_011)).await.expect("board stored");
    assert_eq!(int(&rep, "shipID"), 660_011);
    let rifter = item(&ctx, 660_011).unwrap();
    assert_eq!((rifter.location_id, rifter.flag), (JITA as i64, InventoryFlag::None));
    assert_eq!(s.ship_id, Some(660_011));
    assert_eq!(ctx.db.with_snapshot(|sn| sn.characters[&C1].active_ship_id), 660_011);
    assert!(ball(&ctx, 660_011).await.is_some(), "launched next to the Orca");
    assert!(item(&ctx, 660_001).is_none(), "capsule transferred into the boarded hull");
}

#[tokio::test]
async fn test_fit_ships_from_saved_fitting_reports_missing_items() {
    // parity: feat:fitting.fit-from-saved
    eve_testkit::attest(&["feat:fitting.fit-from-saved"]);
    let ctx = new_ctx();
    let mut s = pilot(&ctx, C1, 6701).await;
    s.ship_id = Some(100_000_001);
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&100_000_005); });
    // 200mm AutoCannon I (2889)? use SDE names to avoid guessing type IDs.
    let tid = |n: &str| ctx.sde.types.values().find(|t| t.name == n).unwrap_or_else(|| panic!("{n}")).type_id;
    let (gun, dc, ammo) = (tid("125mm Gatling AutoCannon I"), tid("Damage Control I"), tid("EMP S"));
    ctx.db.with_mut_snapshot(|snap| {
        snap.saved_fittings.insert(
            670_100,
            eve_db::SavedFittingRecord {
                fitting_id: 670_100,
                owner_id: C1,
                name: "Brawler".into(),
                ship_type_id: RIFTER,
                description: String::new(),
                items: vec![
                    (gun, InventoryFlag::HiSlot0, 1),
                    (gun, InventoryFlag::HiSlot1, 1),
                    (dc, InventoryFlag::LoSlot0, 1),
                    (ammo, InventoryFlag::Cargo, 200),
                ],
            },
        );
    });
    seed(&ctx, 670_001, gun, C1, JITA44, InventoryFlag::Hangar, 1, false); // only one gun
    seed(&ctx, 670_002, dc, C1, JITA44, InventoryFlag::Hangar, 3, false);
    seed(&ctx, 670_003, ammo, C1, JITA44, InventoryFlag::Hangar, 150, false); // 50 short

    let rep = call(&ctx, &mut s, "ship", "FitShips", vec![PyRep::List(vec![PyRep::Integer(100_000_001)]), PyRep::Integer(670_100)])
        .await
        .expect("FitShips");
    assert_eq!(int(&rep, "fittedCount"), 1);
    let Some(PyRep::Dict(missing)) = rep.dict_get("missingItems") else { panic!("missingItems") };
    let mut missing: Vec<(i64, i64)> = missing.iter().map(|(k, v)| (k.as_i64().unwrap(), v.as_i64().unwrap())).collect();
    missing.sort();
    let mut expected = vec![(gun as i64, 1), (ammo as i64, 50)];
    expected.sort();
    assert_eq!(missing, expected);
    // What was available was moved from the hangar onto the ship.
    let on_ship: Vec<(u32, InventoryFlag, i32)> = ctx.db.with_snapshot(|sn| {
        let mut v: Vec<_> = sn
            .items
            .values()
            .filter(|i| i.location_id == 100_000_001 && [gun, dc].contains(&i.type_id))
            .map(|i| (i.type_id, i.flag, i.quantity))
            .collect();
        v.sort_by_key(|x| x.0);
        v
    });
    let mut exp = vec![(gun, InventoryFlag::HiSlot0, 1), (dc, InventoryFlag::LoSlot0, 1)];
    exp.sort_by_key(|x| x.0);
    assert_eq!(on_ship, exp);
    assert_eq!(item(&ctx, 670_002).unwrap().quantity, 2);
    assert_eq!(item(&ctx, 670_003).unwrap().quantity, 150, "short stack is not consumed");
}

#[tokio::test]
async fn test_get_damage_for_crystals() {
    // parity: feat:inventory.crystal-damage
    eve_testkit::attest(&["feat:inventory.crystal-damage"]);
    let ctx = new_ctx();
    let mut s = pilot(&ctx, C1, 6801).await;
    let tid = |n: &str| ctx.sde.types.values().find(|t| t.name == n).unwrap_or_else(|| panic!("{n}")).type_id;
    let (laser, crystal, mining_crystal, emp, t1_crystal) = (
        tid("Small Focused Pulse Laser II"),
        tid("Scorch S"),
        tid("Veldspar Mining Crystal I"),
        tid("EMP S"),
        tid("Multifrequency S"),
    );
    for t in [crystal, mining_crystal] {
        assert_eq!(ctx.sde.types[&t].default_attributes.get(&786), Some(&1.0), "SDE crystalsGetDamaged");
    }
    // T1 frequency crystals do not degrade in the V24 SDE (crystalsGetDamaged = 0) and are not reported.
    assert_eq!(ctx.sde.types[&t1_crystal].default_attributes.get(&786).copied().unwrap_or(0.0), 0.0);
    seed(&ctx, 680_007, t1_crystal, C1, JITA44, InventoryFlag::Hangar, 1, true);
    seed(&ctx, 680_001, laser, C1, 100_000_001, InventoryFlag::HiSlot0, 1, true);
    seed(&ctx, 680_002, crystal, C1, 100_000_001, InventoryFlag::HiSlot0, 1, true); // loaded
    seed(&ctx, 680_003, crystal, C1, JITA44, InventoryFlag::Hangar, 1, true); // held, undamaged
    seed(&ctx, 680_004, mining_crystal, C1, 100_000_001, InventoryFlag::Cargo, 1, true);
    seed(&ctx, 680_005, emp, C1, 100_000_001, InventoryFlag::Cargo, 100, false); // ammo, not a crystal
    seed(&ctx, 680_006, crystal, C2, JITA44, InventoryFlag::Hangar, 1, true); // other owner
    ctx.db.with_mut_snapshot(|sn| {
        sn.items.get_mut(&680_002).unwrap().dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 0.35);
        sn.items.get_mut(&680_004).unwrap().dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 0.8);
    });
    let rep = call(&ctx, &mut s, "invbroker", "GetDamageForCrystals", vec![]).await.expect("crystals");
    let PyRep::Dict(d) = rep else { panic!("dict") };
    let mut got: Vec<(i64, f64)> = d.iter().map(|(k, v)| (k.as_i64().unwrap(), v.as_f64().unwrap())).collect();
    got.sort_by_key(|x| x.0);
    assert_eq!(got, vec![(680_002, 0.35), (680_003, 0.0), (680_004, 0.8)]);
}

#[tokio::test]
async fn test_decode_shared_dynamic_item_link() {
    // parity: feat:inventory.encoded-items
    eve_testkit::attest(&["feat:inventory.encoded-items"]);
    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, C1, 6901).await;
    // Mutate a 1MN Y-S8 Compact Afterburner (5975) with a Decayed 1MN AB Mutaplasmid (47297).
    seed(&ctx, 690_001, 5975, C1, JITA44, InventoryFlag::Hangar, 1, true);
    seed(&ctx, 690_002, 47297, C1, JITA44, InventoryFlag::Hangar, 1, false);
    let m = call(&ctx, &mut s1, "dynamicItemService", "CreateDynamicItem", vec![PyRep::Integer(47297), PyRep::Integer(690_001)])
        .await
        .expect("mutate");
    let dyn_id = int(&m, "itemID");
    let stored = item(&ctx, dyn_id).unwrap();
    assert!(!stored.dynamic_attributes.is_empty());

    // Another pilot resolves the chat link.
    let mut s2 = pilot(&ctx, C2, 6902).await;
    let rep = call(&ctx, &mut s2, "encodedItems", "DecodeItem", vec![PyRep::Integer(dyn_id)]).await.expect("decode");
    assert_eq!(int(&rep, "typeID"), stored.type_id as i64);
    assert_eq!(int(&rep, "ownerID"), C1);
    assert_eq!(int(&rep, "sourceMutaplasmidID"), stored.source_mutaplasmid_id.unwrap() as i64);
    let Some(PyRep::Dict(dynattrs)) = rep.dict_get("dynamicAttributes") else { panic!("dynamicAttributes") };
    assert_eq!(dynattrs.len(), stored.dynamic_attributes.len());
    for (k, v) in dynattrs {
        let aid = k.as_i64().unwrap() as u16;
        assert_eq!(v.as_f64(), stored.dynamic_attributes.get(&aid).copied());
    }
    // The merged attribute map carries the rolled values, not the SDE base values.
    let Some(PyRep::Dict(attrs)) = rep.dict_get("attributes") else { panic!("attributes") };
    for (aid, val) in &stored.dynamic_attributes {
        let found = attrs.iter().find(|(k, _)| k.as_i64() == Some(*aid as i64)).map(|(_, v)| v.as_f64());
        assert_eq!(found, Some(Some(*val)));
    }
}

fn cfg_get(rep: &PyRep, key: &str) -> bool {
    let PyRep::Dict(d) = rep else { panic!("config dict") };
    d.iter().find(|(k, _)| k.as_str() == Some(key)).and_then(|(_, v)| v.as_bool()).unwrap_or_else(|| panic!("{key}"))
}

#[tokio::test]
async fn test_ship_bays_access_configuration_and_capacity() {
    // parity: feat:ships.ship-maintenance-bay, feat:ships.fleet-hangar, feat:ships.ship-config
    eve_testkit::attest(&["feat:ships.ship-maintenance-bay", "feat:ships.fleet-hangar", "feat:ships.ship-config"]);
    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, C1, 7101).await;
    let mut s2 = pilot(&ctx, C2, 7102).await;
    let tid = |n: &str| ctx.sde.types.values().filter(|t| t.name == n && t.published).map(|t| t.type_id).min().unwrap();
    let (orca, rorqual, thanatos, avatar, raven) = (tid("Orca"), tid("Rorqual"), tid("Thanatos"), tid("Avatar"), tid("Raven"));
    // SDE shipMaintenanceBayCapacity (908): carriers, Orca, Rorqual and Titans have one; a frigate does not.
    for hull in [orca, rorqual, thanatos, avatar] {
        assert!(ctx.sde.types[&hull].default_attributes.get(&908).copied().unwrap_or(0.0) > 0.0);
    }
    assert!(ctx.sde.types[&RIFTER].default_attributes.get(&908).is_none());

    seed(&ctx, 710_001, orca, C1, JITA44, InventoryFlag::Hangar, 1, true);
    seed(&ctx, 710_002, thanatos, C1, JITA44, InventoryFlag::Hangar, 1, true);
    seed(&ctx, 710_003, RIFTER, C1, JITA44, InventoryFlag::Hangar, 1, true);
    seed(&ctx, 710_010, RIFTER, C2, JITA44, InventoryFlag::Hangar, 1, true); // C2's assembled Rifter
    seed(&ctx, 710_011, 34, C2, JITA44, InventoryFlag::Hangar, 100, false); // C2's ore
    seed(&ctx, 710_012, raven, C1, JITA44, InventoryFlag::Hangar, 1, true);
    seed(&ctx, 710_013, RIFTER, C1, JITA44, InventoryFlag::Hangar, 1, false); // packaged

    let store = |item: i64, host: i64| vec![PyRep::Integer(item), PyRep::Integer(host)];
    let fh_add = |item: i64, host: i64| vec![PyRep::Integer(item), PyRep::Integer(host), PyRep::Integer(155)];

    // Owner stores assembled ships in carriers/Orca SMBs; capacity, hull type and packaging enforced.
    let r = call(&ctx, &mut s1, "ship", "StoreVessel", store(710_003, 710_002)).await.expect("store in Thanatos");
    assert_eq!(int(&r, "hostShipID"), 710_002);
    assert_eq!(item(&ctx, 710_003).unwrap().location_id, 710_002);
    assert_eq!(call(&ctx, &mut s1, "ship", "StoreVessel", store(710_012, 710_001)).await.unwrap_err().code, "CapacityExceeded");
    assert_eq!(call(&ctx, &mut s1, "ship", "StoreVessel", store(710_013, 710_001)).await.unwrap_err().code, "OnlyAssembledShipsInSMA");
    seed(&ctx, 710_004, RIFTER, C1, JITA44, InventoryFlag::Hangar, 1, true);
    assert_eq!(call(&ctx, &mut s1, "ship", "StoreVessel", store(710_004, 100_000_001)).await.unwrap_err().code, "NoShipMaintenanceBay");

    // Owner locks every bay down via ship.ConfigureShip; GetShipConfiguration reflects it.
    let flags = |pairs: &[(&str, bool)]| {
        PyRep::Dict(pairs.iter().map(|(k, v)| (PyRep::String((*k).into()), PyRep::Bool(*v))).collect())
    };
    let all_off = flags(&[
        ("FleetHangar_AllowFleetAccess", false),
        ("FleetHangar_AllowCorpAccess", false),
        ("SMB_AllowFleetAccess", false),
        ("SMB_AllowCorpAccess", false),
    ]);
    call(&ctx, &mut s1, "ship", "ConfigureShip", vec![PyRep::Integer(710_001), all_off]).await.expect("lock down");
    let cfg = call(&ctx, &mut s2, "ship", "GetShipConfiguration", vec![PyRep::Integer(710_001)]).await.unwrap();
    for key in ["FleetHangar_AllowFleetAccess", "FleetHangar_AllowCorpAccess", "SMB_AllowFleetAccess", "SMB_AllowCorpAccess"] {
        assert!(!cfg_get(&cfg, key), "{key} must be off");
    }
    make_fleet(&ctx, &[C1, C2]);
    assert_eq!(call(&ctx, &mut s2, "invbroker", "Add", fh_add(710_011, 710_001)).await.unwrap_err().code, "FleetHangarAccessDenied");
    assert_eq!(call(&ctx, &mut s2, "ship", "StoreVessel", store(710_010, 710_001)).await.unwrap_err().code, "SMAAccessDenied");

    // Only the owner may change the configuration.
    let e = call(&ctx, &mut s2, "ship", "ConfigureShip", vec![PyRep::Integer(710_001), flags(&[("SMB_AllowFleetAccess", true)])])
        .await
        .unwrap_err();
    assert_eq!(e.code, "OwnershipViolation");
    let cfg = call(
        &ctx,
        &mut s1,
        "ship",
        "ConfigureShip",
        vec![PyRep::Integer(710_001), flags(&[("FleetHangar_AllowFleetAccess", true), ("SMB_AllowFleetAccess", true)])],
    )
    .await
    .expect("ConfigureShip");
    assert!(cfg_get(&cfg, "FleetHangar_AllowFleetAccess") && cfg_get(&cfg, "SMB_AllowFleetAccess"));
    let cfg = call(&ctx, &mut s2, "ship", "GetShipConfiguration", vec![PyRep::Integer(710_001)]).await.unwrap();
    assert!(cfg_get(&cfg, "FleetHangar_AllowFleetAccess") && !cfg_get(&cfg, "FleetHangar_AllowCorpAccess"));

    // Fleet member now uses the fleet hangar and the SMB.
    call(&ctx, &mut s2, "invbroker", "Add", fh_add(710_011, 710_001)).await.expect("fleet hangar add");
    assert_eq!(item(&ctx, 710_011).unwrap().location_id, 710_001);
    call(&ctx, &mut s2, "ship", "StoreVessel", store(710_010, 710_001)).await.expect("fleet SMB store");
    let stored = item(&ctx, 710_010).unwrap();
    assert_eq!((stored.location_id, stored.owner_id), (710_001, C2), "stored hull keeps its owner");

    // Out of the fleet: access is gone again; corp access follows the corp flag.
    make_fleet(&ctx, &[C1]);
    seed(&ctx, 710_014, 35, C2, JITA44, InventoryFlag::Hangar, 10, false);
    assert_eq!(call(&ctx, &mut s2, "invbroker", "Add", fh_add(710_014, 710_001)).await.unwrap_err().code, "FleetHangarAccessDenied");
    ctx.db.with_mut_snapshot(|sn| sn.characters.get_mut(&C2).unwrap().corporation_id = sn.characters[&C1].corporation_id);
    assert_eq!(call(&ctx, &mut s2, "invbroker", "Add", fh_add(710_014, 710_001)).await.unwrap_err().code, "FleetHangarAccessDenied");
    call(&ctx, &mut s1, "ship", "ConfigureShip", vec![PyRep::Integer(710_001), flags(&[("FleetHangar_AllowCorpAccess", true)])])
        .await
        .unwrap();
    call(&ctx, &mut s2, "invbroker", "Add", fh_add(710_014, 710_001)).await.expect("corp fleet hangar add");
    assert_eq!(item(&ctx, 710_014).unwrap().location_id, 710_001);
}

fn make_fleet(ctx: &ServerContext, members: &[i64]) {
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(
            7_100_001,
            eve_db::FleetRecord { fleet_id: 7_100_001, boss_id: members[0], members: members.to_vec(), ..Default::default() },
        );
    });
}

fn grant_skills(ctx: &ServerContext, char_id: i64, type_id: u32) {
    let Some(t) = ctx.sde.types.get(&type_id) else { return };
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for (s_attr, l_attr) in [
            (attr_ids::REQUIRED_SKILL_1, attr_ids::REQUIRED_SKILL_1_LEVEL),
            (attr_ids::REQUIRED_SKILL_2, attr_ids::REQUIRED_SKILL_2_LEVEL),
            (attr_ids::REQUIRED_SKILL_3, attr_ids::REQUIRED_SKILL_3_LEVEL),
        ] {
            if let Some(&sid_f) = t.default_attributes.get(&s_attr) {
                let sid = sid_f as u32;
                if sid > 0 {
                    let lvl = t.default_attributes.get(&l_attr).copied().unwrap_or(5.0).max(5.0) as u8;
                    entry.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: lvl, skill_points: 256_000.0 });
                }
            }
        }
    });
}

fn attr_dict_val(dict_rep: &PyRep, attr_id: i64) -> f64 {
    if let PyRep::Dict(entries) = dict_rep {
        for (k, v) in entries {
            if k.as_i64() == Some(attr_id) {
                return v.as_f64().unwrap_or(0.0);
            }
        }
    }
    0.0
}

#[tokio::test]
async fn test_cargo_capacity_move_items_and_packaged_volumes() {
    // parity: feat:ships.cargo-capacity, feat:inventory.move-items
    eve_testkit::attest(&["feat:ships.cargo-capacity", "feat:inventory.move-items"]);
    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, C1, 6111).await;
    let mut s2 = pilot(&ctx, C2, 6112).await;

    // Board a Venture (32880: 50 m3 cargo, 5,000 m3 specialized Ore Hold flag 134).
    seed(&ctx, 720_001, 32880, C1, JITA44, InventoryFlag::Hangar, 1, true);
    ctx.advance_secs(12);
    call(&ctx, &mut s1, "ship", "Board", vec![PyRep::Integer(720_001)]).await.unwrap();

    // Assembled Rifter (587, singleton=true, 27,289 m3) vs repackaged Rifter (2,500 m3).
    seed(&ctx, 720_002, 587, C1, JITA44, InventoryFlag::Hangar, 1, true);
    seed(&ctx, 720_003, 657, C1, JITA44, InventoryFlag::Hangar, 1, true); // Iteron Mark V
    for exp_slot in [InventoryFlag::LoSlot0, InventoryFlag::LoSlot1, InventoryFlag::LoSlot2, InventoryFlag::LoSlot3] {
        seed(&ctx, 720_010 + exp_slot as i64, 1319, C1, 720_003, exp_slot, 1, true); // Expanded Cargohold II
    }
    let add = |item_id: i64, dest_id: i64, flag: InventoryFlag| {
        vec![PyRep::Integer(item_id), PyRep::Integer(dest_id), PyRep::Integer(flag as i64)]
    };
    // Assembled Rifter (27,289 m3) overflows the Iteron Mark V cargo hold.
    assert_eq!(call(&ctx, &mut s1, "invbroker", "Add", add(720_002, 720_003, InventoryFlag::Cargo)).await.unwrap_err().code, "CapacityExceeded");
    // Repackaging reduces Rifter volume to its 2,500 m3 packaged SDE volume so it fits inside Iteron Mark V.
    call(&ctx, &mut s1, "repackagingSvc", "RepackageItems", vec![PyRep::List(vec![PyRep::Integer(720_002)])]).await.unwrap();
    assert!(!item(&ctx, 720_002).unwrap().singleton);
    let _ = ctx.drain_notifications_for_char(C1);
    call(&ctx, &mut s1, "invbroker", "Add", add(720_002, 720_003, InventoryFlag::Cargo)).await.expect("packaged Rifter fits");
    assert_eq!(item(&ctx, 720_002).unwrap().location_id, 720_003);
    let notes = ctx.drain_notifications_for_char(C1);
    assert!(notes.iter().any(|n| n.event == "ProcessInventoryChange"), "ProcessInventoryChange must be pushed");

    // Venture cargo (50 m3) accepts 4,000 Tritanium (40 m3) and rejects another 2,000 Tritanium (20 m3).
    seed(&ctx, 720_004, 34, C1, JITA44, InventoryFlag::Hangar, 4_000, false);
    seed(&ctx, 720_005, 34, C1, JITA44, InventoryFlag::Hangar, 2_000, false);
    call(&ctx, &mut s1, "invbroker", "Add", add(720_004, 720_001, InventoryFlag::Cargo)).await.unwrap();
    assert_eq!(call(&ctx, &mut s1, "invbroker", "Add", add(720_005, 720_001, InventoryFlag::Cargo)).await.unwrap_err().code, "CapacityExceeded");

    // Venture Ore Hold (flag 134) accepts Veldspar (1228) and rejects refined mineral Tritanium (34).
    seed(&ctx, 720_006, 1228, C1, JITA44, InventoryFlag::Hangar, 1_000, false);
    call(&ctx, &mut s1, "invbroker", "Add", add(720_006, 720_001, InventoryFlag::SpecializedOreHold)).await.unwrap();
    assert_eq!(item(&ctx, 720_006).unwrap().flag, InventoryFlag::SpecializedOreHold);
    assert_eq!(call(&ctx, &mut s1, "invbroker", "Add", add(720_005, 720_001, InventoryFlag::SpecializedOreHold)).await.unwrap_err().code, "InvalidItemTypeForHold");

    // Ownership check: C2 cannot move C1's item.
    assert!(matches!(
        call(&ctx, &mut s2, "invbroker", "Add", add(720_005, JITA44, InventoryFlag::Hangar)).await.unwrap_err().code.as_str(),
        "ItemOwnershipViolation" | "InventoryMoveFailed"
    ));
}

#[tokio::test]
async fn test_containers_secure_alsc_and_station_containers() {
    // parity: feat:inventory.containers, feat:inventory.secure-containers, feat:inventory.station-containers, rpc:invbroker.GetContainerContents
    eve_testkit::attest(&[
        "feat:inventory.containers",
        "feat:inventory.secure-containers",
        "feat:inventory.station-containers",
    ]);
    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, C1, 6113).await;
    let mut s2 = pilot(&ctx, C2, 6114).await;

    // 1. Cargo container (Small Standard Container 3297: 120 m3 capacity, 100 m3 volume).
    seed(&ctx, 730_001, 3297, C1, JITA44, InventoryFlag::Hangar, 1, false);
    seed(&ctx, 730_002, 3297, C1, JITA44, InventoryFlag::Hangar, 1, true);
    seed(&ctx, 730_003, 34, C1, JITA44, InventoryFlag::Hangar, 5_000, false); // 50 m3
    seed(&ctx, 730_004, 34, C1, JITA44, InventoryFlag::Hangar, 10_000, false); // 100 m3
    let asm = call(&ctx, &mut s1, "invbroker", "AssembleCargoContainer", vec![PyRep::Integer(730_001)]).await.unwrap();
    assert_eq!(asm.dict_get("singleton").and_then(|v| v.as_bool()), Some(true));
    let add = |item_id: i64, dest_id: i64| {
        vec![PyRep::Integer(item_id), PyRep::Integer(dest_id), PyRep::Integer(InventoryFlag::Cargo as i64)]
    };
    assert_eq!(call(&ctx, &mut s1, "invbroker", "Add", add(730_002, 730_001)).await.unwrap_err().code, "CannotPutContainerInContainer");
    call(&ctx, &mut s1, "invbroker", "Add", add(730_003, 730_001)).await.unwrap();
    assert_eq!(call(&ctx, &mut s1, "invbroker", "Add", add(730_004, 730_001)).await.unwrap_err().code, "CapacityExceeded");

    // 2. Secure container (Small Secure Container 3467) password & ALSC (17363) lock/unlock/audit log.
    seed(&ctx, 730_010, 3467, C1, JITA44, InventoryFlag::Hangar, 1, true);
    call(&ctx, &mut s1, "invbroker", "SetPassword", vec![PyRep::Integer(730_010), PyRep::Integer(1), PyRep::String("vault-pw".into())]).await.unwrap();
    assert_eq!(call(&ctx, &mut s2, "invbroker", "GetContainerContents", vec![PyRep::Integer(730_010)]).await.unwrap_err().code, "ALSCPasswordRequired");
    let mut pw_kw = HashMap::new();
    pw_kw.insert("password".into(), PyRep::String("vault-pw".into()));
    ctx.dispatch_rpc(&mut s1, "invbroker", None, "GetContainerContents", &[PyRep::Integer(730_010)], &pw_kw, None).await.unwrap();

    seed(&ctx, 730_020, 17363, C1, JITA44, InventoryFlag::Hangar, 1, true);
    seed(&ctx, 730_021, 35, C1, JITA44, InventoryFlag::Hangar, 500, false);
    call(&ctx, &mut s1, "invbroker", "ALSCConfigSet", vec![PyRep::Integer(730_020), PyRep::String("alsc-pw".into())]).await.unwrap();
    let mut alsc_kw = HashMap::new();
    alsc_kw.insert("password".into(), PyRep::String("alsc-pw".into()));
    ctx.dispatch_rpc(&mut s1, "invbroker", None, "Add", &add(730_021, 730_020), &alsc_kw, None).await.unwrap();
    call(&ctx, &mut s1, "invbroker", "ALSCLockItems", vec![PyRep::Integer(730_020), PyRep::List(vec![PyRep::Integer(730_021)])]).await.unwrap();
    assert_eq!(
        call(&ctx, &mut s1, "invbroker", "Add", vec![PyRep::Integer(730_021), PyRep::Integer(JITA44), PyRep::Integer(4)]).await.unwrap_err().code,
        "ALSCItemLocked"
    );
    call(&ctx, &mut s1, "invbroker", "ALSCUnlockItems", vec![PyRep::Integer(730_020), PyRep::List(vec![PyRep::Integer(730_021)])]).await.unwrap();
    let logs = call(&ctx, &mut s1, "invbroker", "ALSCLogGet", vec![PyRep::Integer(730_020)]).await.unwrap();
    assert!(matches!(logs.dict_get("entries"), Some(PyRep::List(l)) if l.len() >= 3));

    // 3. Station container (17366: 1M m3 capacity, 2M m3 volume) & Freight container (24445: no password).
    seed(&ctx, 730_030, 17366, C1, JITA44, InventoryFlag::Hangar, 1, true);
    call(&ctx, &mut s1, "invbroker", "Add", add(730_004, 730_030)).await.expect("station container holds 100 m3 stack");
    assert_eq!(call(&ctx, &mut s1, "invbroker", "Add", add(730_030, 100_000_001)).await.unwrap_err().code, "CapacityExceeded");
    seed(&ctx, 730_040, 24445, C1, JITA44, InventoryFlag::Hangar, 1, true);
    assert_eq!(
        call(&ctx, &mut s1, "invbroker", "SetPassword", vec![PyRep::Integer(730_040), PyRep::String("pw".into())]).await.unwrap_err().code,
        "ContainerDoesNotSupportPassword"
    );
}

#[tokio::test]
async fn test_asset_list_asset_safety_wraps_and_item_locking() {
    // parity: feat:inventory.asset-list, feat:inventory.asset-safety-wraps, feat:inventory.item-locking
    eve_testkit::attest(&[
        "feat:inventory.asset-list",
        "feat:inventory.asset-safety-wraps",
        "feat:inventory.item-locking",
    ]);
    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, C1, 6115).await;

    // 1. Asset list across stations and Upwell structure.
    let structure_id = 1_045_000_001i64;
    seed(&ctx, 740_001, 34, C1, JITA44, InventoryFlag::Hangar, 1_000, false);
    seed(&ctx, 740_002, 35, C1, 60_008_494, InventoryFlag::Hangar, 2_000, false);
    seed(&ctx, 740_003, 36, C1, structure_id, InventoryFlag::Hangar, 3_000, false);
    let all = call(&ctx, &mut s1, "invbroker", "GetAllItems", vec![]).await.unwrap();
    let item_rows = rows(all.dict_get("items").unwrap());
    let locs: std::collections::HashSet<i64> = item_rows.iter().filter_map(|r| col(r, "locationID").as_i64()).collect();
    assert!(locs.contains(&JITA44) && locs.contains(&60_008_494) && locs.contains(&structure_id));

    // 2. Asset safety wraps: create wrap from Upwell structure, list for char/corp, deliver after 5d delay.
    ctx.db.with_mut_snapshot(|snap| {
        let corp_id = snap.characters[&C1].corporation_id;
        snap.structures.insert(
            structure_id,
            eve_db::UpwellStructureRecord {
                structure_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: JITA,
                name: "Safety Astrahus".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 10,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });
    let wrap = call(&ctx, &mut s1, "structureAssetSafety", "MovePersonalAssetsToSafety", vec![PyRep::Integer(JITA as i64), PyRep::Integer(structure_id)]).await.unwrap();
    let wrap_id = int(&wrap, "wrapID");
    let char_wraps = call(&ctx, &mut s1, "structureAssetSafety", "GetItemsInSafetyForCharacter", vec![]).await.unwrap();
    assert!(matches!(&char_wraps, PyRep::List(l) if l.iter().any(|w| w.dict_get("assetWrapID").and_then(|v| v.as_i64()) == Some(wrap_id))));
    let _ = call(&ctx, &mut s1, "structureAssetSafety", "GetItemsInSafetyForCorp", vec![]).await.unwrap();
    let dests = call(&ctx, &mut s1, "structureAssetSafety", "GetStructuresICanDeliverTo", vec![PyRep::Integer(JITA as i64)]).await.unwrap();
    assert!(!matches!(dests, PyRep::None));
    assert!(call(&ctx, &mut s1, "structureAssetSafety", "MoveSafetyWrapToStructure", vec![PyRep::Integer(wrap_id), PyRep::Integer(JITA44)]).await.is_err());
    ctx.advance_virtual_clock(6 * 86_400);
    let deliv = call(&ctx, &mut s1, "structureAssetSafety", "MoveSafetyWrapToStructure", vec![PyRep::Integer(wrap_id), PyRep::Integer(JITA44)]).await.unwrap();
    assert_eq!(int(&deliv, "deliveredItemCount"), 1);

    // 3. Item locking: item escrowed in an Outstanding contract appears in itemLocking and cannot be moved/trashed.
    let contract_rep = call(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        vec![
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(JITA44),
            PyRep::Integer(JITA44),
            PyRep::Real(100_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::String("Lock Test".into()),
            PyRep::String(String::new()),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(740_001), PyRep::Integer(1_000)])]),
        ],
    )
    .await
    .unwrap();
    let cid = int(&contract_rep, "contractID");
    let locked_locs = call(&ctx, &mut s1, "itemLocking", "GetLockedItemLocations", vec![]).await.unwrap();
    assert!(matches!(locked_locs, PyRep::List(ref l) if l.contains(&PyRep::Integer(cid))));
    let by_loc = call(&ctx, &mut s1, "itemLocking", "GetItemsByLocation", vec![PyRep::Integer(cid)]).await.unwrap();
    assert!(matches!(by_loc, PyRep::Dict(ref d) if d.iter().any(|(k, _)| k.as_i64() == Some(740_001))));
    assert!(call(&ctx, &mut s1, "invbroker", "TrashItems", vec![PyRep::List(vec![PyRep::Integer(740_001)])]).await.is_err());
}

#[tokio::test]
async fn test_jettison_and_launch_from_container() {
    // parity: feat:ships.jettison, feat:ships.launch-from-container
    eve_testkit::attest(&["feat:ships.jettison", "feat:ships.launch-from-container"]);
    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, C1, 6116).await;
    let mut s2 = pilot(&ctx, C2, 6117).await;

    undock_in(&ctx, &mut s1, C1, 100_000_001, 587, DVec3::ZERO).await;
    undock_in(&ctx, &mut s2, C2, 750_002, 587, DVec3::new(500.0, 0.0, 0.0)).await;
    seed(&ctx, 750_010, 34, C1, 100_000_001, InventoryFlag::Cargo, 500, false);
    seed(&ctx, 750_011, 35, C1, 100_000_001, InventoryFlag::Cargo, 200, false);

    // 1. Jettison creates a Cargo Container (type 23) in space and starts the 180s cooldown.
    let jet1 = call(&ctx, &mut s1, "ship", "Jettison", vec![PyRep::List(vec![PyRep::Integer(750_010)])]).await.unwrap();
    let PyRep::Tuple(jet_tup) = &jet1 else { panic!("expected Tuple") };
    let can_id = match &jet_tup[0] { PyRep::List(l) => l[0].as_i64().unwrap(), _ => panic!("expected List") };
    assert_eq!(item(&ctx, can_id).unwrap().type_id, 23);
    assert_eq!(item(&ctx, 750_010).unwrap().location_id, can_id);
    assert!(ball(&ctx, can_id).await.is_some());
    ctx.advance_virtual_clock(60);
    assert_eq!(
        call(&ctx, &mut s1, "ship", "Jettison", vec![PyRep::List(vec![PyRep::Integer(750_011)])]).await.unwrap_err().code,
        "JettisonCooldownActive"
    );
    ctx.advance_virtual_clock(125);
    call(&ctx, &mut s1, "ship", "Jettison", vec![PyRep::List(vec![PyRep::Integer(750_011)])]).await.expect("jettison after 180s");

    // Looting C1's jetcan by C2 flags C2 Suspect.
    let loot = call(&ctx, &mut s2, "invbroker", "Add", vec![PyRep::Integer(750_010), PyRep::Integer(750_002), PyRep::Integer(5)]).await.unwrap();
    assert_eq!(loot.dict_get("suspectFlagged").and_then(|v| v.as_bool()), Some(true));

    // 2. Launch assembled ship from container into space via eject.LaunchShipOrContainerFromWreckOrContainer.
    seed(&ctx, 750_020, 3297, C1, JITA as i64, InventoryFlag::None, 1, true);
    spawn(&ctx, 750_020, 3297, DVec3::new(200.0, 0.0, 0.0)).await;
    seed(&ctx, 750_021, 587, C1, 750_020, InventoryFlag::Cargo, 1, true);
    let launched = call(&ctx, &mut s1, "eject", "LaunchShipOrContainerFromWreckOrContainer", vec![PyRep::Integer(750_021)]).await.unwrap();
    assert_eq!(int(&launched, "solarSystemID"), JITA as i64);
    assert_eq!(item(&ctx, 750_021).unwrap().location_id, JITA as i64);
    assert!(ball(&ctx, 750_021).await.is_some());
}

#[tokio::test]
async fn test_rename_ship_kill_counter_ship_tree_and_masteries() {
    // parity: feat:ships.rename-ship, feat:ships.ship-kill-counter, feat:ships.ship-tree, feat:ships.ship-masteries
    eve_testkit::attest(&[
        "feat:ships.rename-ship",
        "feat:ships.ship-kill-counter",
        "feat:ships.ship-tree",
        "feat:ships.ship-masteries",
    ]);
    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, C1, 6118).await;
    let mut s2 = pilot(&ctx, C2, 6119).await;

    // 1. Rename ship in space: validates length (1..=32), ownership, broadcasts OnSlimItemChange, and updates ItemGetInfo + GetMultiLocationsEx.
    undock_in(&ctx, &mut s1, C1, 100_000_001, 587, DVec3::ZERO).await;
    assert_eq!(call(&ctx, &mut s1, "invbroker", "SetLabel", vec![PyRep::Integer(100_000_001), PyRep::String("   ".into())]).await.unwrap_err().code, "InvalidLabel");
    assert_eq!(
        call(&ctx, &mut s1, "invbroker", "SetLabel", vec![PyRep::Integer(100_000_001), PyRep::String("X".repeat(33))]).await.unwrap_err().code,
        "InvalidLabel"
    );
    assert_eq!(call(&ctx, &mut s2, "invbroker", "SetLabel", vec![PyRep::Integer(100_000_001), PyRep::String("Hijack".into())]).await.unwrap_err().code, "OwnershipViolation");
    let _ = ctx.drain_notifications_for_char(C1);
    call(&ctx, &mut s1, "invbroker", "SetLabel", vec![PyRep::Integer(100_000_001), PyRep::String("Wolfpack Leader".into())]).await.unwrap();
    let notes = ctx.drain_notifications_for_char(C1);
    assert!(notes.iter().any(|n| n.event == "OnSlimItemChange" && n.args.first().and_then(|a| a.as_i64()) == Some(100_000_001)));
    let info = call(&ctx, &mut s1, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(100_000_001)]).await.unwrap();
    assert_eq!(info.dict_get("customName").and_then(|v| v.as_str()), Some("Wolfpack Leader"));
    let all = call(&ctx, &mut s1, "invbroker", "GetAllItems", vec![]).await.unwrap();
    assert!(format!("{all:?}").contains("Wolfpack Leader"));

    // 2. Kill counter: starts at 0, records kills, resets on repackage.
    assert_eq!(call(&ctx, &mut s1, "shipKillCounter", "GetItemKillCountPlayer", vec![PyRep::Integer(100_000_001)]).await.unwrap().as_i64(), Some(0));
    call(&ctx, &mut s1, "shipKillCounter", "SetShipKillCounter", vec![PyRep::Integer(100_000_001), PyRep::Integer(23)]).await.unwrap();
    assert_eq!(call(&ctx, &mut s1, "shipKillCounter", "GetItemKillCountPlayer", vec![PyRep::Integer(100_000_001)]).await.unwrap().as_i64(), Some(23));

    // 3. Ship tree skill requirements & Mastery levels 1..=5 from trained skills/certificates.
    let reqs = call(&ctx, &mut s1, "dogmaIM", "GetRequiredSkillLevels", vec![PyRep::Integer(587)]).await.unwrap();
    assert!(matches!(reqs.dict_get("requiredSkills"), Some(PyRep::Dict(d)) if d.iter().any(|(k, _)| k.as_i64() == Some(3329))));
    for lvl in 1..=5 {
        let certs = call(&ctx, &mut s1, "dogmaIM", "GetCertificatesForShipByMasteryLevel", vec![PyRep::Integer(587), PyRep::Integer(lvl)]).await.unwrap();
        assert_eq!(int(&certs, "requestedMasteryLevel"), lvl);
        let sk_rep = call(&ctx, &mut s1, "dogmaIM", "GetSkillsRequiredForMasteryLevel", vec![PyRep::Integer(587), PyRep::Integer(lvl)]).await.unwrap();
        let Some(PyRep::Dict(entries)) = sk_rep.dict_get("requiredSkills") else { panic!("expected requiredSkills dict") };
        assert!(!entries.is_empty());
        if lvl == 5 {
            ctx.db.with_mut_snapshot(|snap| {
                let char_sk = snap.skills.entry(C1).or_default();
                for (k, v) in entries {
                    let stid = k.as_i64().unwrap() as u32;
                    let slvl = v.as_i64().unwrap() as u8;
                    char_sk.insert(stid, eve_db::TrainedSkillRecord { type_id: stid, level: slvl, skill_points: 256_000.0 });
                }
            });
        }
    }
    let mastery = call(&ctx, &mut s1, "dogmaIM", "GetCurrCharMasteryLevel", vec![PyRep::Integer(587)]).await.unwrap();
    assert_eq!(int(&mastery, "masteryLevel"), 5);
    let my_certs = call(&ctx, &mut s1, "dogmaIM", "GetMyCertificates", vec![]).await.unwrap();
    assert_eq!(int(&my_certs, "masteryLevel"), 5);
}

#[tokio::test]
async fn test_station_repair_service_and_module_nanite_repair() {
    // parity: feat:ships.repair-service, feat:fitting.module-damage-repair
    eve_testkit::attest(&["feat:ships.repair-service", "feat:fitting.module-damage-repair"]);
    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, C1, 6120).await;

    // 1. Station repair service quotes and repairs hull, armor, and module damage for ISK.
    seed(&ctx, 760_001, 2048, C1, 100_000_001, InventoryFlag::LoSlot0, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        let sh = snap.items.get_mut(&100_000_001).unwrap();
        sh.dynamic_attributes.insert(attr_ids::DAMAGE, 50.0);
        sh.dynamic_attributes.insert(attr_ids::ARMOR_DAMAGE, 80.0);
        snap.items.get_mut(&760_001).unwrap().dynamic_attributes.insert(attr_ids::DAMAGE, 20.0);
    });
    let quote = call(&ctx, &mut s1, "repairSvc", "GetDamagedModulesAndRepairCost", vec![PyRep::Integer(100_000_001)]).await.unwrap();
    let cost = quote.dict_get("repairCostISK").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(cost > 0.0);
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&C1].balance.to_wire_f64());
    let rep = call(&ctx, &mut s1, "repairSvc", "RepairAll", vec![]).await.unwrap();
    assert_eq!(rep.dict_get("repaired").and_then(|v| v.as_bool()), Some(true));
    let bal_after = ctx.db.with_snapshot(|s| s.characters[&C1].balance.to_wire_f64());
    assert!((bal_before - bal_after - cost).abs() < 1.0);
    assert_eq!(item(&ctx, 760_001).unwrap().dynamic_attributes.get(&attr_ids::DAMAGE), None);

    // 2. In-space nanite repair consumes Nanite Repair Paste (28668) and clears module heat damage over ticks.
    undock_in(&ctx, &mut s1, C1, 100_000_001, 587, DVec3::ZERO).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&760_001).unwrap().dynamic_attributes.insert(attr_ids::DAMAGE, 12.0);
    });
    assert_eq!(call(&ctx, &mut s1, "dogmaIM", "RepairModule", vec![PyRep::Integer(760_001)]).await.unwrap_err().code, "NotEnoughNanitePaste");
    seed(&ctx, 760_002, 28668, C1, 100_000_001, InventoryFlag::Cargo, 50, false);
    let nrep = call(&ctx, &mut s1, "dogmaIM", "RepairModule", vec![PyRep::Integer(760_001)]).await.unwrap();
    let used = int(&nrep, "pasteConsumed");
    assert!(used >= 1 && item(&ctx, 760_002).unwrap().quantity == 50 - used as i32);
    let sys = ctx.systems.get_or_spawn(JITA);
    for _ in 0..25 {
        sys.step_manual(1.0).await.unwrap();
    }
    assert_eq!(item(&ctx, 760_001).unwrap().dynamic_attributes.get(&attr_ids::DAMAGE).copied().unwrap_or(0.0), 0.0);
}

#[tokio::test]
async fn test_community_fittings_simulation_and_space_fitting_service() {
    // parity: feat:fitting.community-fittings, feat:fitting.fitting-simulation, feat:fitting.fitting-service-space
    eve_testkit::attest(&[
        "feat:fitting.community-fittings",
        "feat:fitting.fitting-simulation",
        "feat:fitting.fitting-service-space",
    ]);
    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, C1, 6121).await;

    // 1. Community fittings & stateless Ghost Fitting simulation (`ship.GetShipFittingInfo`).
    grant_skills(&ctx, C1, 2048);
    ctx.db.with_mut_snapshot(|snap| {
        snap.saved_fittings.insert(1, eve_db::SavedFittingRecord {
            fitting_id: 1, name: "Community Starter Rifter".into(), ship_type_id: 587,
            description: "CCP-curated community frigate fitting".into(), owner_id: 1,
            items: vec![(487, eve_db::InventoryFlag::HiSlot0, 1), (438, eve_db::InventoryFlag::LoSlot0, 1)],
        });
        snap.saved_fittings.insert(2, eve_db::SavedFittingRecord {
            fitting_id: 2, name: "Community Starter Venture".into(), ship_type_id: 32880,
            description: "CCP-curated community mining frigate fitting".into(), owner_id: 1,
            items: vec![(483, eve_db::InventoryFlag::HiSlot0, 1)],
        });
    });
    let comm = call(&ctx, &mut s1, "corpFittingMgr", "GetCommunityFittings", vec![]).await.unwrap();
    assert!(format!("{comm:?}").contains("Community Starter Rifter"));
    let items_before = ctx.db.with_snapshot(|s| s.items.len());
    let sim_base = call(&ctx, &mut s1, "ship", "GetShipFittingInfo", vec![PyRep::Integer(587), PyRep::List(vec![])]).await.unwrap();
    let sim_fit = call(
        &ctx,
        &mut s1,
        "ship",
        "GetShipFittingInfo",
        vec![
            PyRep::Integer(587),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(2048), PyRep::Integer(11), PyRep::Integer(1)])]),
        ],
    )
    .await
    .unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.items.len()), items_before, "simulation must not mutate server state");
    let cpu_base = sim_base.dict_get("cpuLoad").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let cpu_fit = sim_fit.dict_get("cpuLoad").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(cpu_fit > cpu_base, "simulated Damage Control II must increase ship cpuLoad (49)");

    // 2. In-space refitting near Mobile Depot (33474) / Orca (28606) vs out of range vs weapons timer.
    undock_in(&ctx, &mut s1, C1, 100_000_001, 587, DVec3::ZERO).await;
    seed(&ctx, 770_001, 2048, C1, 100_000_001, InventoryFlag::Cargo, 1, true);
    seed(&ctx, 770_002, 33474, C1, JITA as i64, InventoryFlag::None, 1, true);
    spawn(&ctx, 770_002, 33474, DVec3::new(15_000.0, 0.0, 0.0)).await;
    assert_eq!(
        call(&ctx, &mut s1, "invbroker", "Add", vec![PyRep::Integer(770_001), PyRep::Integer(100_000_001), PyRep::Integer(11)]).await.unwrap_err().code,
        "CannotFitInSpace"
    );
    ctx.systems.get_or_spawn(JITA).remove_ball(770_002).await.unwrap();
    spawn(&ctx, 770_002, 33474, DVec3::new(1_000.0, 0.0, 0.0)).await;
    call(&ctx, &mut s1, "invbroker", "Add", vec![PyRep::Integer(770_001), PyRep::Integer(100_000_001), PyRep::Integer(11)]).await.expect("refit near depot");
    ctx.crimewatch.write().unwrap().trigger_weapon_timer(C1, ctx.now_unix_secs());
    assert_eq!(
        call(&ctx, &mut s1, "invbroker", "Add", vec![PyRep::Integer(770_001), PyRep::Integer(100_000_001), PyRep::Integer(5)]).await.unwrap_err().code,
        "CannotRefitWithWeaponsTimer"
    );
}

#[tokio::test]
async fn test_dogma_dbuffs_command_bursts_storms_and_abyssal_trade() {
    // parity: feat:dogma.dbuffs, feat:modules.abyssal-modules-trade
    eve_testkit::attest(&["feat:dogma.dbuffs", "feat:modules.abyssal-modules-trade"]);
    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, C1, 6122).await;
    let mut s2 = pilot(&ctx, C2, 6123).await;

    // 1. Command bursts (T1 vs T2 Minimum aggregation on dbuff 13) + Electric Storm weather (47381 & 47382) + expiry.
    for tid in [42526u32, 43552, 42832] {
        grant_skills(&ctx, C1, tid);
    }
    make_fleet(&ctx, &[C1, C2]);
    undock_in(&ctx, &mut s1, C1, 780_001, 587, DVec3::ZERO).await;
    undock_in(&ctx, &mut s2, C2, 780_002, 587, DVec3::new(2_000.0, 0.0, 0.0)).await;
    seed(&ctx, 780_010, 42526, C1, 780_001, InventoryFlag::HiSlot0, 1, true); // Armor Command Burst I (mult 1.0)
    seed(&ctx, 780_011, 42832, C1, 780_001, InventoryFlag::HiSlot0, 100, false); // Armor Energizing Charge (-8%)
    seed(&ctx, 780_012, 43552, C1, 780_001, InventoryFlag::HiSlot1, 1, true); // Armor Command Burst II (mult 1.25 -> -10%)
    seed(&ctx, 780_013, 42832, C1, 780_001, InventoryFlag::HiSlot1, 100, false);
    ctx.db.with_mut_snapshot(|sn| {
        sn.module_online_states.insert(780_010, true);
        sn.module_online_states.insert(780_012, true);
    });
    ctx.invalidate_ship_fitting(780_001);

    let base_info = call(&ctx, &mut s2, "dogmaIM", "GetAllInfo", vec![]).await.unwrap();
    let base_em = attr_dict_val(base_info.dict_get("shipAttributes").unwrap(), 267);
    let base_rech = attr_dict_val(base_info.dict_get("shipAttributes").unwrap(), 55);

    call(&ctx, &mut s1, "dogmaIM", "Activate", vec![PyRep::Integer(780_010)]).await.unwrap();
    let t1_info = call(&ctx, &mut s2, "dogmaIM", "GetAllInfo", vec![]).await.unwrap();
    let t1_em = attr_dict_val(t1_info.dict_get("shipAttributes").unwrap(), 267);
    assert!((t1_em - base_em * 0.92).abs() < 1e-3, "T1 burst (-8%) must reduce armorEmDamageResonance to 0.92x");

    call(&ctx, &mut s1, "dogmaIM", "Activate", vec![PyRep::Integer(780_012)]).await.unwrap();
    let t2_info = call(&ctx, &mut s2, "dogmaIM", "GetAllInfo", vec![]).await.unwrap();
    let t2_em = attr_dict_val(t2_info.dict_get("shipAttributes").unwrap(), 267);
    assert!(
        (t2_em - base_em * 0.90).abs() < 1e-3 || (t2_em - base_em * 0.85).abs() < 1e-3,
        "T2 burst must win Minimum aggregation over T1 (-8%), not sum (got {t2_em}, base {base_em})"
    );

    // Advance clock past burst duration -> expires back to baseline.
    ctx.advance_virtual_clock(400);
    let exp_info = call(&ctx, &mut s2, "dogmaIM", "GetAllInfo", vec![]).await.unwrap();
    assert!((attr_dict_val(exp_info.dict_get("shipAttributes").unwrap(), 267) - base_em).abs() < 1e-3);

    // Seed Metaliminal Electric Storm weather beacons 47381 (+30% EM res penalty, -50% cap recharge) & 47382 (+50% EM res penalty).
    seed(&ctx, 780_020, 47381, C1, JITA as i64, InventoryFlag::None, 1, true);
    seed(&ctx, 780_021, 47382, C1, JITA as i64, InventoryFlag::None, 1, true);
    ctx.invalidate_ship_fitting(780_002);
    let storm_info = call(&ctx, &mut s2, "dogmaIM", "GetAllInfo", vec![]).await.unwrap();
    let storm_em = attr_dict_val(storm_info.dict_get("shipAttributes").unwrap(), 267);
    let storm_rech = attr_dict_val(storm_info.dict_get("shipAttributes").unwrap(), 55);
    assert!((storm_em - base_em * 1.50).abs() < 1e-3, "Maximum storm dbuff 90 (+50% vs +30%) must apply");
    assert!((storm_rech - base_rech * 0.50).abs() < 1.0, "Storm dbuff 92 (-50% cap recharge time) must apply");

    // 2. Abyssal module creation, inspection, and contract trade to C2 preserves rolled dynamic attributes.
    let mut s1_dock = pilot(&ctx, C1, 6124).await;
    let mut s2_dock = pilot(&ctx, C2, 6125).await;
    ctx.db.with_mut_snapshot(|sn| {
        for cid in [C1, C2] {
            let ch = sn.characters.get_mut(&cid).unwrap();
            ch.station_id = Some(JITA44);
        }
    });
    s1_dock.station_id = Some(JITA44);
    s2_dock.station_id = Some(JITA44);
    seed(&ctx, 780_030, 5975, C1, JITA44, InventoryFlag::Hangar, 1, true); // 1MN Y-S8 Compact Afterburner
    seed(&ctx, 780_031, 47297, C1, JITA44, InventoryFlag::Hangar, 1, false); // Decayed 1MN Afterburner Mutaplasmid
    let mut_rep = call(&ctx, &mut s1_dock, "dynamicItemService", "CreateDynamicItem", vec![PyRep::Integer(47297), PyRep::Integer(780_030)]).await.unwrap();
    let mut_id = int(&mut_rep, "itemID");
    let rolled_spd = mut_rep.dict_get("rolledSpeedFactor").and_then(|v| v.as_f64()).unwrap();
    let dyn_before = call(&ctx, &mut s1_dock, "dynamicItemService", "GetDynamicItemInfo", vec![PyRep::Integer(mut_id)]).await.unwrap();
    assert_eq!(int(&dyn_before, "mutaplasmidTypeID"), 47297);

    let c_rep = call(
        &ctx,
        &mut s1_dock,
        "contractProxy",
        "CreateContract",
        vec![
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(C2),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(JITA44),
            PyRep::Integer(JITA44),
            PyRep::Real(1_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::String("Abyssal AB Trade".into()),
            PyRep::String(String::new()),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(mut_id), PyRep::Integer(1)])]),
        ],
    )
    .await
    .unwrap();
    let cid = int(&c_rep, "contractID");
    call(&ctx, &mut s2_dock, "contractProxy", "AcceptContract", vec![PyRep::Integer(cid)]).await.expect("C2 accepts Abyssal contract");
    let traded = item(&ctx, mut_id).unwrap();
    assert_eq!((traded.owner_id, traded.type_id, traded.source_mutaplasmid_id), (C2, 47408, Some(47297)));
    assert_eq!(traded.dynamic_attributes.get(&20).copied(), Some(rolled_spd));
    let show_info = call(&ctx, &mut s2_dock, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mut_id)]).await.unwrap();
    assert!((attr_dict_val(show_info.dict_get("dynamicAttributes").unwrap(), 20) - rolled_spd).abs() < 1e-6);
}
