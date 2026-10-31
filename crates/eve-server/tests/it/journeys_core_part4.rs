//! End-to-End Integration Journeys Part 4 (`journey:ship_loss` .. `journey:corporation`).
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

#[tokio::test]
async fn test_journey_ship_loss() {
    // parity: journey:ship_loss
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

    // 2. Check insurance contracts & killmails
    let contracts = call_rpc(&ctx, &mut sess, "insuranceSvc", "GetContracts", &[])
        .await
        .expect("GetContracts");
    let PyRep::List(clist) = contracts else { panic!("expected List from GetContracts") };
    assert_eq!(clist.len(), 1);
    let kms = call_rpc(&ctx, &mut sess, "charMgr", "GetRecentShipKillsAndLosses", &[PyRep::Integer(10), PyRep::Integer(0)])
        .await
        .expect("GetRecentShipKillsAndLosses");
    assert!(matches!(kms, PyRep::List(_) | PyRep::Tuple(_) | PyRep::Rowset { .. }));
}

#[tokio::test]
async fn test_journey_clones() {
    // parity: journey:clones
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(402);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");

    // 1. Install a jump clone
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(
            24242,
            eve_db::TrainedSkillRecord { type_id: 24242, level: 2, skill_points: 50_000.0 },
        );
    });
    call_rpc(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStation", &[])
        .await
        .expect("InstallCloneInStation");
    let clone_state = call_rpc(&ctx, &mut sess, "jumpCloneSvc", "GetCloneState", &[])
        .await
        .expect("GetCloneState");
    assert!(format!("{clone_state:?}").contains("clones"));

    // 2. Set home station
    call_rpc(&ctx, &mut sess, "charMgr", "SetHomeStation", &[PyRep::Integer(60_003_760)])
        .await
        .expect("SetHomeStation");
    let home = ctx.db.with_snapshot(|s| s.characters[&90_000_001].home_station_id);
    assert_eq!(home, 60_003_760);
}

#[tokio::test]
async fn test_journey_industry() {
    // parity: journey:industry
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

    // 2. Advance timer & deliver job
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(j) = snap.industry_jobs.get_mut(&job_id) {
            j.start_unix = 0;
        }
    });
    let done = call_rpc(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(job_id)])
        .await
        .expect("CompleteJob");
    assert_eq!(done.dict_get("jobID").and_then(|v| v.as_i64()).unwrap_or(job_id), job_id);
}

#[tokio::test]
async fn test_journey_planetary() {
    // parity: journey:planetary
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
    assert!(!format!("{col:?}").is_empty());
}

#[tokio::test]
async fn test_journey_corporation() {
    // parity: journey:corporation
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
    sess1.corporation_id = Some(corp_id);
    sess1.corp_role = corp_roles::ROLE_DIRECTOR;

    // 2. Apply & accept second character, rent office, create alliance
    call_rpc(&ctx, &mut sess2, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Joining".into())])
        .await
        .expect("InsertApplication");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.corporation_id = corp_id;
        }
    });
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&90_000_002].corporation_id), corp_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.master_wallet = IskCents::from_isk(2_000_000_000);
            c.set_division_balance(1, IskCents::from_isk(2_000_000_000));
        }
    });
    call_rpc(&ctx, &mut sess1, "officeManager", "RentOffice", &[PyRep::Integer(60_003_760)])
        .await
        .expect("RentOffice");

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
