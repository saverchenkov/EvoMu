//! Integration regression tests covering blocking findings B-01 through B-15.

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_blocking_findings_b01_to_b15_regressions() {
    // parity: rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID, rpc:industryManager.CompleteJob, rpc:industryManager.InstallJob, rpc:planetMgr.Flush, rpc:planetMgr.UserUpdateNetwork, rpc:shipCosmeticsMgr.ActivateSkinLicense, rpc:shipCosmeticsMgr.ApplySkinToShip
    use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();
    let mut sess = SessionState::new(1);
    let kw = HashMap::new();

    // B-07 & AP-05: Production WorldRepository::with_demo_seed() has 0 GM accounts, only starter items 100_000_001..=100_000_012, and 0 items in 100_000_013..=100_000_028
    for starter_id in 100_000_001..=100_000_012 {
        assert!(
            ctx.db.get_item(starter_id).is_some(),
            "Expected starter item {starter_id} in with_demo_seed()"
        );
    }
    for test_only_item_id in 100_000_013..=100_000_028 {
        assert!(
            ctx.db.get_item(test_only_item_id).is_none(),
            "Production seed must not contain test-only fixture item {test_only_item_id}"
        );
    }

    // Seed expansion test fixtures explicitly inside the test harness
    ctx.db.with_mut_snapshot(|snap| {
        let extra_items = [
            (
                100_000_015,
                35832,
                100_000_001,
                InventoryFlag::Cargo,
                5,
                false,
            ),
            (
                100_000_016,
                56201,
                100_000_001,
                InventoryFlag::Cargo,
                5,
                false,
            ),
            (
                100_000_020,
                20424,
                60_003_760,
                InventoryFlag::Hangar,
                10,
                false,
            ),
            (
                100_000_028,
                20172,
                60_003_760,
                InventoryFlag::Hangar,
                10,
                false,
            ),
            (
                100_000_023,
                23055,
                100_000_024,
                InventoryFlag::FighterBay,
                9,
                false,
            ),
            (
                100_000_024,
                24483,
                60_003_760,
                InventoryFlag::Hangar,
                1,
                true,
            ),
            (
                100_000_025,
                4247,
                100_000_024,
                InventoryFlag::Cargo,
                500,
                false,
            ),
            (
                100_000_026,
                4247,
                100_000_001,
                InventoryFlag::Cargo,
                2_000,
                false,
            ),
            (
                100_000_027,
                34748,
                60_003_760,
                InventoryFlag::Hangar,
                1,
                false,
            ),
            (
                100_000_036,
                35840,
                100_000_001,
                InventoryFlag::Cargo,
                2,
                false,
            ),
        ];
        for (item_id, type_id, location_id, flag, quantity, singleton) in extra_items {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id: 90_000_001,
                    location_id,
                    flag,
                    quantity,
                    singleton,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
        let char_skills = snap.skills.entry(90_000_001).or_default();
        for (skill_id, level, sp) in [
            (3388, 4, 45_255.0),
            (11448, 4, 226_275.0),
            (11452, 4, 226_275.0),
            (21791, 4, 226_275.0),
            (24242, 4, 45_255.0),
            (33407, 4, 90_510.0),
            (21611, 2, 12_699.0),
        ] {
            char_skills.insert(
                skill_id,
                TrainedSkillRecord {
                    type_id: skill_id,
                    level,
                    skill_points: sp,
                },
            );
        }
    });

    let gm_fail = ctx
        .dispatch_rpc(
            &mut sess,
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("admin".into()),
                PyRep::String("admin-secret".into()),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(gm_fail.unwrap_err().code, "AuthenticationFailed");

    // Login as demo capsuleer (90_000_001)
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login demo");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // R-P1-06: non-V24 air_npe.AdvanceNode is removed and returns UnimplementedRPC
    let npe_gate_err = ctx
        .dispatch_rpc(&mut sess, "air_npe", None, "AdvanceNode", &[], &kw, None) // rpc-lint: allow legacy/negative-test RPC (air_npe.AdvanceNode)
        .await;
    assert_eq!(npe_gate_err.unwrap_err().code, "UnimplementedRPC");

    // ApplySkinToShip rejects unlicensed SKIN before ActivateSkinLicense, and ActivateSkinLicense consumes item 100_000_027
    let unlicensed = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCosmeticsMgr",
            None,
            "ApplySkinToShip",
            &[PyRep::Integer(587), PyRep::Integer(34748)],
            &kw,
            None,
        )
        .await;
    assert_eq!(unlicensed.unwrap_err().code, "SkinNotLicensed");

    ctx.dispatch_rpc(
        &mut sess,
        "shipCosmeticsMgr",
        None,
        "ActivateSkinLicense",
        &[PyRep::Integer(34748)],
        &kw,
        None,
    )
    .await
    .expect("ActivateSkinLicense");
    let no_second_license = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCosmeticsMgr",
            None,
            "ActivateSkinLicense",
            &[PyRep::Integer(34748)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        no_second_license.unwrap_err().code,
        "SkinLicenseItemNotFound"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "shipCosmeticsMgr",
        None,
        "ApplySkinToShip",
        &[PyRep::Integer(587), PyRep::Integer(34748)],
        &kw,
        None,
    )
    .await
    .expect("ApplySkinToShip after license activation");

    // B-04: Installing T2 Invention (activity 8) before copying any BPC from BPO 691 must fail with InventionRequiresBPC
    let bpo_inv_err = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::Integer(691), PyRep::Integer(8), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await;
    assert_eq!(bpo_inv_err.unwrap_err().code, "InventionRequiresBPC");

    // Copy Rifter BPO (691) twice (producing 2 BPCs with is_blueprint_copy=true), then test deterministic invention failure and success via server-side override
    let copy1 = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::Integer(691), PyRep::Integer(5), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("Copy BPO 1");
    let PyRep::KeyVal(copy1_kv) = copy1 else {
        panic!("Expected copy1 KeyVal");
    };
    let copy1_id = copy1_kv.get("jobID").unwrap().as_i64().unwrap() as u64;
    ctx.advance_virtual_clock(10_000);
    ctx.dispatch_rpc(
        &mut sess,
        "industryManager",
        None,
        "CompleteJob",
        &[PyRep::Integer(copy1_id as i64)],
        &kw,
        None,
    )
    .await
    .expect("Complete Copy 1");

    let copy2 = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::Integer(691), PyRep::Integer(5), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("Copy BPO 2");
    let PyRep::KeyVal(copy2_kv) = copy2 else {
        panic!("Expected copy2 KeyVal");
    };
    let copy2_id = copy2_kv.get("jobID").unwrap().as_i64().unwrap() as u64;
    ctx.advance_virtual_clock(10_000);
    ctx.dispatch_rpc(
        &mut sess,
        "industryManager",
        None,
        "CompleteJob",
        &[PyRep::Integer(copy2_id as i64)],
        &kw,
        None,
    )
    .await
    .expect("Complete Copy 2");

    // Install invention 1 and test failure roll (server-side override = 0.95 >= 0.4100)
    let inv1 = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::Integer(691), PyRep::Integer(8), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("Install invention 1");
    let PyRep::KeyVal(inv1_kv) = inv1 else {
        panic!("Expected inv1 KeyVal");
    };
    let inv1_job_id = inv1_kv.get("jobID").unwrap().as_i64().unwrap() as u64;
    ctx.advance_virtual_clock(70_000);
    ctx.set_invention_roll_override(Some(0.95));
    let inv1_fail = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "CompleteJob",
            &[PyRep::Integer(inv1_job_id as i64)],
            &kw,
            None,
        )
        .await;
    assert_eq!(inv1_fail.unwrap_err().code, "InventionAttemptFailed");

    // Install invention 2 and test success roll (server-side override = 0.10 < 0.4100)
    let inv2 = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::Integer(691), PyRep::Integer(8), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("Install invention 2");
    let PyRep::KeyVal(inv2_kv) = inv2 else {
        panic!("Expected inv2 KeyVal");
    };
    let inv2_job_id = inv2_kv.get("jobID").unwrap().as_i64().unwrap() as u64;
    ctx.advance_virtual_clock(70_000);
    ctx.set_invention_roll_override(Some(0.10));
    let inv2_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "CompleteJob",
            &[PyRep::Integer(inv2_job_id as i64)],
            &kw,
            None,
        )
        .await
        .expect("Complete invention 2");
    ctx.set_invention_roll_override(None);
    let PyRep::KeyVal(inv2_ok_kv) = inv2_ok else {
        panic!("Expected inv2_ok KeyVal");
    };
    assert_eq!(
        inv2_ok_kv.get("productTypeID").and_then(|v| v.as_i64()),
        Some(11372)
    );

    // planetMgr.UserUpdateNetwork + planetMgr.Flush materializes 200x Water (type_id 3645) into station hangar
    let ship_id = sess.ship_id.unwrap_or(100_000_001);
    let prev_st = sess.station_id;
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
        }
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            eve_db::InventoryItemRecord {
                item_id: id,
                type_id: 2534,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
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
        "UserUpdateNetwork",
        &[
            PyRep::Integer(40009082),
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
    .expect("planetMgr.UserUpdateNetwork");
    sess.station_id = prev_st;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = prev_st;
        }
    });
    let flush = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "Flush",
            &[PyRep::Integer(40009082)],
            &kw,
            None,
        )
        .await
        .expect("planetMgr.Flush");
    let PyRep::KeyVal(flush_kv) = flush else {
        panic!("Expected flush KeyVal");
    };
    let delivered_water_id = flush_kv.get("deliveredItemID").unwrap().as_i64().unwrap() as u64;
    assert!(delivered_water_id > 0);
}
