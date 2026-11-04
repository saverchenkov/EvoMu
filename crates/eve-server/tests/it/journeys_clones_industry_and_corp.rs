//! End-to-End Integration Journeys: Ship Loss, Clones, Industry, Planetary, Corporation.
//!
//! Covering:
//! - `journey:ship_loss`
//! - `journey:clones`
//! - `journey:industry`
//! - `journey:planetary`
//! - `journey:corporation`

#![forbid(unsafe_code)]

use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

async fn call_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

// parity: journey:ship_loss
#[tokio::test]
async fn test_journey_ship_loss() {
    eve_testkit::attest(&["journey:ship_loss"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(401);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");

    let ship_id = sess.ship_id.expect("ship_id");
    // 1. Insure ship
    let ins = call_rpc(&ctx, &mut sess, "insuranceSvc", "InsureShip", &[PyRep::Integer(ship_id), PyRep::Real(1.0)])
        .await
        .expect("InsureShip");
    assert!(ins.dict_get("payoutISK").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

    // 2. Check insurance contracts, undock, destroy ship, and verify capsule ejection, insurance payout, wreck, and killmail
    let contracts = call_rpc(&ctx, &mut sess, "insuranceSvc", "GetContracts", &[])
        .await
        .expect("GetContracts");
    let PyRep::List(clist) = contracts else { panic!("expected List from GetContracts") };
    assert_eq!(clist.len(), 1);

    call_rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("ship.Undock");
    let isk_before = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
    let sd1 = call_rpc(&ctx, &mut sess, "ship", "SelfDestruct", &[])
        .await
        .expect("ship.SelfDestruct start");
    assert_eq!(sd1.dict_get("countdownStarted"), Some(&PyRep::Bool(true)));
    ctx.advance_secs(125);
    let pod_id = ctx.db.with_snapshot(|s| s.characters[&90_000_001].active_ship_id);
    assert_ne!(pod_id, ship_id, "pilot must be ejected into a capsule after ship destruction");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&pod_id].type_id), 670);
    let wreck_exists = ctx.db.with_snapshot(|s| s.items.values().any(|i| i.type_id == 26557 || ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 186)));
    assert!(wreck_exists, "ship destruction must spawn a wreck");
    let isk_after = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
    assert!(isk_after > isk_before, "insurance payout must credit victim wallet");

    let kms = call_rpc(&ctx, &mut sess, "charMgr", "GetRecentShipKillsAndLosses", &[PyRep::Integer(10), PyRep::Integer(0)])
        .await
        .expect("GetRecentShipKillsAndLosses");
    let PyRep::Rowset { rows: km_rows, .. } = kms else {
        panic!("expected Rowset from GetRecentShipKillsAndLosses");
    };
    assert_eq!(km_rows.len(), 1);
    assert!(format!("{km_rows:?}").contains("90000001"));
}

// parity: journey:clones
#[tokio::test]
async fn test_journey_clones() {
    eve_testkit::attest(&["journey:clones"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(402);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");

    // 1. Train Infomorph Psychology (24242) + Cybernetics (3411), plug in implant (9899), install jump clone
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(90_000_001).or_default();
        sk.insert(24242, eve_db::TrainedSkillRecord { type_id: 24242, level: 2, skill_points: 50_000.0 });
        sk.insert(3411, eve_db::TrainedSkillRecord { type_id: 3411, level: 2, skill_points: 50_000.0 });
    });
    let imp_id = ctx.db.add_or_stack_item(90_000_001, 60_003_760, InventoryFlag::Hangar, 9899, 1).unwrap().item_id;
    call_rpc(&ctx, &mut sess, "dogmaIM", "InjectImplant", &[PyRep::Integer(imp_id)])
        .await
        .expect("InjectImplant");
    call_rpc(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStation", &[])
        .await
        .expect("InstallCloneInStation");
    let jc_id = ctx.db.with_snapshot(|s| s.jump_clones[&90_000_001][0].jump_clone_id);

    // 2. Leave ship into capsule, move to station 60_003_757, jump back to clone in 60_003_760 via CloneJump
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
    let active_ship = sess.ship_id.unwrap();
    call_rpc(&ctx, &mut sess, "ship", "LeaveShip", &[PyRep::Integer(active_ship)])
        .await
        .expect("LeaveShip into capsule");
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&90_000_001).unwrap().station_id = Some(60_003_757));
    sess.station_id = Some(60_003_757);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
    call_rpc(&ctx, &mut sess, "jumpCloneSvc", "CloneJump", &[PyRep::Integer(jc_id)])
        .await
        .expect("CloneJump");
    assert_eq!(sess.station_id, Some(60_003_760));
    let clone_state = call_rpc(&ctx, &mut sess, "jumpCloneSvc", "GetCloneState", &[])
        .await
        .expect("GetCloneState");
    assert!(format!("{clone_state:?}").contains("jumpCloneID"));

    // 3. Set home station via V24 home_station.set_home_station & self-destruct medical clone in capsule
    call_rpc(&ctx, &mut sess, "home_station", "set_home_station", &[PyRep::Integer(60_003_760)])
        .await
        .expect("home_station.set_home_station");
    let home = ctx.db.with_snapshot(|s| s.characters[&90_000_001].home_station_id);
    assert_eq!(home, 60_003_760);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
    call_rpc(&ctx, &mut sess, "home_station", "self_destruct_clone", &[])
        .await
        .expect("home_station.self_destruct_clone");
    assert_eq!(sess.station_id, Some(60_003_760));
}

// parity: journey:industry
#[tokio::test]
async fn test_journey_industry() {
    eve_testkit::attest(&["journey:industry"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(403);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");

    // Seed Rifter BPO (691) and Tritanium (34)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_000_601,
            InventoryItemRecord {
                item_id: 100_000_601,
                type_id: 691,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    ctx.db.add_or_stack_item(90_000_001, 60_003_760, InventoryFlag::Hangar, 34, 50_000).unwrap();

    // 1. Install manufacturing job
    let job_rep = call_rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(691), PyRep::Integer(1), PyRep::Integer(1)])
        .await
        .expect("InstallJob Manufacturing");
    let job_id = job_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("jobID");
    let dur_s = job_rep.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap_or(600) as u64;

    // 2. Advance virtual clock past job duration & deliver job
    ctx.advance_secs(dur_s + 10);
    let done = call_rpc(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(job_id)])
        .await
        .expect("CompleteJob");
    assert_eq!(done.dict_get("jobID").and_then(|v| v.as_i64()).unwrap_or(job_id), job_id);
    assert!(
        ctx.db.with_snapshot(|s| s.items.values().any(|i| i.owner_id == 90_000_001 && i.type_id == 587)),
        "CompleteJob must deliver manufactured Rifter (type 587)"
    );
}

// parity: journey:planetary
#[tokio::test]
async fn test_journey_planetary() {
    eve_testkit::attest(&["journey:planetary"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(404);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");

    let planet_id = 40_009_082_i64;
    let ship_id = sess.ship_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(90_000_001).or_default();
        for sid in [2495_u32, 2505, 2403, 2406] {
            skills.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
    });
    ctx.db.add_or_stack_item(90_000_001, ship_id, InventoryFlag::Cargo, 2534, 1).unwrap(); // Gas Command Center
    call_rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("ship.Undock");

    // 1. Build colony with command center, launchpad, and link
    let res = call_rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(planet_id),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(1001), PyRep::Integer(2534), PyRep::Real(0.1), PyRep::Real(0.1)]),
                PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(1001), PyRep::Integer(3)]),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(1002), PyRep::Integer(2543), PyRep::Real(0.11), PyRep::Real(0.11)]),
                PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(1001), PyRep::Integer(1002), PyRep::Integer(0)]),
            ]),
        ],
    )
    .await
    .expect("UserUpdateNetwork");
    let PyRep::List(pins) = res.dict_get("pins").expect("pins") else { panic!("expected pins list") };
    assert_eq!(pins.len(), 2);

    // 2. Advance time & verify colony state
    ctx.advance_secs(3_600);
    let col = call_rpc(&ctx, &mut sess, "planetMgr", "GetPlanetResourceInfo", &[PyRep::Integer(planet_id)])
        .await
        .expect("GetPlanetResourceInfo");
    assert!(col.dict_get("planetID").is_some() || col.dict_get("resources").is_some());
}

// parity: journey:corporation
#[tokio::test]
async fn test_journey_corporation() {
    eve_testkit::attest(&["journey:corporation"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess1 = SessionState::new(405);
    crate::common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID 1");

    let mut sess2 = SessionState::new(406);
    crate::common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    call_rpc(&ctx, &mut sess2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)])
        .await
        .expect("SelectCharacterID 2");

    // 1. Create corporation
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_002);
    let created = call_rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Pioneer Logistics".into()), PyRep::String("PION".into()), PyRep::Real(0.05)],
    )
    .await
    .expect("AddCorporation");
    let corp_id = created.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corporationID");
    assert_eq!(sess1.corporation_id, Some(corp_id));
    assert!(sess1.corp_role & corp_roles::ROLE_DIRECTOR != 0);

    // 2. Apply & accept second character via real corpRegistry RPCs, rent office, create alliance
    let app = call_rpc(&ctx, &mut sess2, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Joining".into())])
        .await
        .expect("InsertApplication");
    let app_id = app
        .dict_get("applicationID")
        .and_then(|v| v.as_i64())
        .or_else(|| app.as_i64())
        .expect("applicationID");
    call_rpc(&ctx, &mut sess1, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())])
        .await
        .expect("UpdateApplicationOffer");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&90_000_002].corporation_id), corp_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.master_wallet = IskCents::from_isk(2_000_000_000);
            c.set_division_balance(1, IskCents::from_isk(2_000_000_000));
        }
    });
    // Negative RentOffice checks: remote station, nonexistent station, insufficient role
    let err_remote = call_rpc(&ctx, &mut sess1, "officeManager", "RentOffice", &[PyRep::Integer(60_003_761)])
        .await
        .expect_err("remote station must fail");
    assert_eq!(err_remote.code, "NotDockedAtStation");
    let orig_station = sess1.station_id;
    sess1.station_id = Some(999_999_999);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&90_000_001) {
            ch.station_id = Some(999_999_999);
        }
    });
    let err_missing = call_rpc(&ctx, &mut sess1, "officeManager", "RentOffice", &[PyRep::Integer(999_999_999)])
        .await
        .expect_err("nonexistent station must fail");
    assert_eq!(err_missing.code, "StationNotFound");
    sess1.station_id = orig_station;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&90_000_001) {
            ch.station_id = orig_station;
        }
    });
    let err_role = call_rpc(&ctx, &mut sess2, "officeManager", "RentOffice", &[PyRep::Integer(60_003_760)])
        .await
        .expect_err("member without RentOffice role must fail");
    assert_eq!(err_role.code, "CorpRoleDenied");

    let corp_wallet_before = ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(1));
    call_rpc(&ctx, &mut sess1, "officeManager", "RentOffice", &[PyRep::Integer(60_003_760)])
        .await
        .expect("RentOffice");
    ctx.db.with_snapshot(|s| {
        assert!(
            s.corporations[&corp_id].division_balance(1) < corp_wallet_before,
            "RentOffice must debit corporation wallet"
        );
        assert!(
            s.items.values().any(|i| i.owner_id == corp_id && i.location_id == 60_003_760 && i.type_id == 27),
            "RentOffice must create office item (type 27) at station"
        );
    });

    let alliance = call_rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "CreateAlliance",
        &[PyRep::String("Pioneer Coalition".into()), PyRep::String("PIONC".into()), PyRep::String("Alliance".into()), PyRep::String("https://evomu.local".into())],
    )
    .await
    .expect("CreateAlliance");
    assert!(alliance.dict_get("allianceID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
}
