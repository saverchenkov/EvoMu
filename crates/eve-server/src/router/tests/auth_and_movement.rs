//! Unit tests for authentication, ownership enforcement, undock double-undock prevention, and movement.

use super::fixtures::*;
use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test]
async fn test_authentication_and_ownership_enforcement() {
    // R22-P1-5: Production ServerContext::new_production() starts with an empty account table and rejects demo credentials
    let prod_ctx = ServerContext::new_production();
    assert!(prod_ctx.db.with_snapshot(|s| s.accounts.is_empty()));
    assert!(prod_ctx.db.authenticate("demo", "demo-secret").is_err());
    let op_uid = prod_ctx
        .db
        .provision_player_account("capsuleer_op", "op-secret-999");
    assert_eq!(op_uid, 1);
    assert!(prod_ctx
        .db
        .authenticate("capsuleer_op", "op-secret-999")
        .is_ok());
    // Verify seed_dual_anchor_markets is idempotent across restarts and does not recreate consumed orders
    assert!(prod_ctx.db.with_snapshot(|s| s.markets_seeded));
    prod_ctx.db.with_mut_snapshot(|s| {
        s.market_orders.remove(&700_000_001);
    });
    let orders_after_consume = prod_ctx.db.with_snapshot(|s| s.market_orders.len());
    assert_eq!(
        crate::world_seeder::seed_dual_anchor_markets(&prod_ctx.db, &prod_ctx.sde),
        0
    );
    assert_eq!(
        prod_ctx.db.with_snapshot(|s| s.market_orders.len()),
        orders_after_consume
    );

    let ctx = ServerContext::new();
    // Deterministic virtual time (T-7): time only moves via advance_virtual_clock.
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let mut session = SessionState::new(1);

    let unauth_res = ctx
        .dispatch_rpc(
            &mut session,
            "charUnboundMgr",
            None,
            "GetCharactersToSelect",
            &[],
            &HashMap::new(),
            None,
        )
        .await;
    assert_eq!(unauth_res.unwrap_err().code, "Unauthorized");

    login_and_select_aura(&ctx, &mut session).await;
    assert_eq!(session.character_id, Some(90_000_001));
    assert_eq!(session.race_id, Some(2));

    let bad_bind = ctx
        .dispatch_rpc(
            &mut session,
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(30_000_999)],
            &HashMap::new(),
            None,
        )
        .await;
    assert_eq!(bad_bind.unwrap_err().code, "BindAuthorizationDenied");

    // Non-admin account ("demo", role 0x01) must be rejected on GM /slash commands
    let slash_denied = ctx
        .dispatch_rpc(
            &mut session,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveisk 1000".into())],
            &HashMap::new(),
            None,
        )
        .await;
    assert_eq!(slash_denied.unwrap_err().code, "InsufficientRole");

    // By default, "admin" is NOT seeded in WorldRepository::with_demo_seed()
    let mut admin_sess = SessionState::new(2);
    let unseeded_admin = ctx
        .dispatch_rpc(
            &mut admin_sess,
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("admin".into()),
                PyRep::String("admin-secret".into()),
            ],
            &HashMap::new(),
            None,
        )
        .await;
    assert_eq!(unseeded_admin.unwrap_err().code, "AuthenticationFailed");

    // Provision GM account explicitly via provision_gm_account (role 0x03) and verify /giveisk
    ctx.db
        .provision_gm_account("admin", "admin-secret", 90_000_003, "ISD Polaris");
    ctx.dispatch_rpc(
        &mut admin_sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("admin".into()),
            PyRep::String("admin-secret".into()),
        ],
        &HashMap::new(),
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut admin_sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_003)],
        &HashMap::new(),
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut admin_sess,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String("/giveisk 1000".into())],
        &HashMap::new(),
        None,
    )
    .await
    .unwrap();

    // Reprocessing non-ore (Tritanium 100_000_011) must be rejected with ItemNotReprocessable
    let bad_reproc = ctx
        .dispatch_rpc(
            &mut session,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(100_000_011)],
            &HashMap::new(),
            None,
        )
        .await;
    assert_eq!(bad_reproc.unwrap_err().code, "ItemNotReprocessable");

    // Jump clone install succeeds when docked, duplicate at same station is rejected,
    // same-station CloneJump places pilot in a Capsule (670) with 0s cooldown, after which Pilot re-boards Rifter 100_000_001
    let clone_res = ctx
        .dispatch_rpc(
            &mut session,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(ckv) = clone_res else {
        panic!("Expected clone KeyVal");
    };
    let jc_id = ckv.get("jumpCloneID").unwrap().as_i64().unwrap();
    let dup_clone = ctx
        .dispatch_rpc(
            &mut session,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &HashMap::new(),
            None,
        )
        .await;
    assert_eq!(dup_clone.unwrap_err().code, "JumpCloneAlreadyAtStation");
    let pod_id = 100_000_099;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            pod_id,
            eve_db::InventoryItemRecord {
                item_id: pod_id,
                type_id: 670,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let ch = snap.characters.get_mut(&90_000_001).unwrap();
        ch.active_ship_id = pod_id;
        ch.balance.0 += 10_000_000_00;
    });
    session.ship_id = Some(pod_id);
    let cj_res = ctx
        .dispatch_rpc(
            &mut session,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(jc_id)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(cj_kv) = cj_res else {
        panic!("Expected CloneJump KeyVal");
    };
    // Same-station clone swap applies 0s cooldown and places character in a Capsule (type_id 670)
    assert_eq!(cj_kv.get("cooldownSeconds"), Some(&PyRep::Integer(0)));
    assert_ne!(session.ship_id, Some(100_000_001));
    // Re-board Rifter 100_000_001 in station hangar via ship.Board
    ctx.advance_secs(12);
    ctx.dispatch_rpc(
        &mut session,
        "ship",
        None,
        "Board",
        &[PyRep::Integer(100_000_001)],
        &HashMap::new(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(session.ship_id, Some(100_000_001));

    // Research ME (activity 4, 10 runs) and TE (activity 3, 10 runs) on Rifter BPO (100_000_007, type 691),
    // copy 2 BPCs (activity 5), and test both failed and successful T2 Invention (activity 8) rolls
    for (act, runs) in [
        (4_i64, 10_i64),
        (3_i64, 10_i64),
        (5_i64, 1_i64),
        (5_i64, 1_i64),
    ] {
        let jr = ctx
            .dispatch_rpc(
                &mut session,
                "industryManager",
                None,
                "InstallJob",
                &[
                    PyRep::Integer(691),
                    PyRep::Integer(act),
                    PyRep::Integer(runs),
                ],
                &HashMap::new(),
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(jkv) = jr else {
            panic!("Expected job KeyVal");
        };
        let jid = jkv.get("jobID").unwrap().as_i64().unwrap();
        ctx.advance_virtual_clock(30_000);
        ctx.dispatch_rpc(
            &mut session,
            "industryManager",
            None,
            "CompleteJob",
            &[PyRep::Integer(jid)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();
    }

    // First T2 Invention job: test premature completion rejection, then deterministic failed roll (override 0.95 >= inv_prob)
    let inv_job_1 = ctx
        .dispatch_rpc(
            &mut session,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::Integer(691), PyRep::Integer(8), PyRep::Integer(1)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(ijkv1) = inv_job_1 else {
        panic!("Expected invention job KeyVal");
    };
    let inv_job_id_1 = ijkv1.get("jobID").unwrap().as_i64().unwrap();
    ctx.advance_virtual_clock(7_200);
    let too_early = ctx
        .dispatch_rpc(
            &mut session,
            "industryManager",
            None,
            "CompleteJob",
            &[PyRep::Integer(inv_job_id_1)],
            &HashMap::new(),
            None,
        )
        .await;
    assert_eq!(too_early.unwrap_err().code, "JobNotYetComplete");
    ctx.advance_virtual_clock(65_000);
    ctx.set_invention_roll_override(Some(0.95));
    let inv_failed = ctx
        .dispatch_rpc(
            &mut session,
            "industryManager",
            None,
            "CompleteJob",
            &[PyRep::Integer(inv_job_id_1)],
            &HashMap::new(),
            None,
        )
        .await;
    assert_eq!(inv_failed.unwrap_err().code, "InventionAttemptFailed");

    // Second T2 Invention job: deterministic successful roll (override 0.10 < inv_prob) yields Wolf Blueprint (11372)
    let inv_job_2 = ctx
        .dispatch_rpc(
            &mut session,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::Integer(691), PyRep::Integer(8), PyRep::Integer(1)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(ijkv2) = inv_job_2 else {
        panic!("Expected invention job 2 KeyVal");
    };
    let inv_job_id_2 = ijkv2.get("jobID").unwrap().as_i64().unwrap();
    ctx.advance_virtual_clock(70_000);
    ctx.set_invention_roll_override(Some(0.10));
    let inv_done = ctx
        .dispatch_rpc(
            &mut session,
            "industryManager",
            None,
            "CompleteJob",
            &[PyRep::Integer(inv_job_id_2)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();
    ctx.set_invention_roll_override(None);
    let PyRep::KeyVal(idkv) = inv_done else {
        panic!("Expected invention completion KeyVal");
    };
    assert_eq!(
        idkv.get("productTypeID"),
        Some(&PyRep::Integer(11372)),
        "Invention of Rifter Blueprint (691) must yield Wolf Blueprint (11372)"
    );
}

#[tokio::test]
async fn test_vertical_slice_undock_double_undock_prevention_and_movement() {
    let ctx = ServerContext::new();
    let mut session = SessionState::new(10);
    login_and_select_aura(&ctx, &mut session).await;

    let undock_res = ctx
        .dispatch_rpc(
            &mut session,
            "stationSvc",
            None,
            "Undock",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();

    let double_undock = ctx
        .dispatch_rpc(
            &mut session,
            "stationSvc",
            None,
            "Undock",
            &[],
            &HashMap::new(),
            None,
        )
        .await;
    assert_eq!(double_undock.unwrap_err().code, "UndockFailed");

    let PyRep::KeyVal(kv) = undock_res else {
        panic!("Expected KeyVal from Undock");
    };
    let Some(PyRep::Integer(bound_id)) = kv.get("beyonceBoundID") else {
        panic!("Expected beyonceBoundID");
    };

    let unbound_call = ctx
        .dispatch_rpc(
            &mut session,
            "beyonce",
            None,
            "CmdGotoDirection",
            &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)],
            &HashMap::new(),
            None,
        )
        .await;
    assert_eq!(unbound_call.unwrap_err().code, "BoundObjectRequired");

    ctx.dispatch_rpc(
        &mut session,
        "beyonce",
        Some(*bound_id as u64),
        "CmdGotoDirection",
        &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)],
        &HashMap::new(),
        None,
    )
    .await
    .unwrap();

    let sys_handle = ctx.systems.get_or_spawn(30_000_142);
    let tick_broadcast = sys_handle.step_manual(1.0).await.unwrap();
    assert!(tick_broadcast.tick >= 2);
}

#[tokio::test]
async fn test_issue_24_bound_object_context_validation_and_lifecycle_revocation() {
    let ctx = ServerContext::new();
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let mut session = SessionState::new(42);
    login_and_select_aura(&ctx, &mut session).await;
    let kw = HashMap::new();

    // 1. Non-GM cannot bind `i2`
    let i2_bind_err = ctx.bind_object(&session, "i2", 90_000_001).unwrap_err();
    assert_eq!(i2_bind_err.code, "BindAuthorizationDenied");

    // 2. Station-bound monikers (`station`, `repairSvc`, station `invbroker`) are valid while docked,
    //    and rejected by `verify_bound_object` / `dispatch_rpc` once undocked into space.
    let station_id = session.station_id.expect("docked at Jita 4-4");
    let st_bid = ctx.bind_object(&session, "station", station_id).unwrap();
    let rep_bid = ctx.bind_object(&session, "repairSvc", station_id).unwrap();
    let inv_st_bid = ctx.bind_object(&session, "invbroker", station_id).unwrap();
    assert!(ctx.verify_bound_object(&session, st_bid).is_ok());
    assert!(ctx.verify_bound_object(&session, rep_bid).is_ok());
    assert!(ctx.verify_bound_object(&session, inv_st_bid).is_ok());

    ctx.advance_secs(12);
    let undock_res = ctx
        .dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(u_kv) = undock_res else {
        panic!("Expected KeyVal from Undock");
    };
    let jita_beyonce_bid = u_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    let jita_scan_bid = ctx.bind_object(&session, "scanMgr", 30_000_142).unwrap();

    // Stale station monikers must now be rejected in space
    assert_eq!(
        ctx.verify_bound_object(&session, st_bid).unwrap_err().code,
        "BoundObjectOwnershipViolation"
    );
    assert_eq!(
        ctx.verify_bound_object(&session, rep_bid).unwrap_err().code,
        "BoundObjectOwnershipViolation"
    );
    assert_eq!(
        ctx.dispatch_rpc(
            &mut session,
            "invbroker",
            Some(inv_st_bid),
            "List",
            &[],
            &kw,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "BoundObjectOwnershipViolation"
    );

    // 3. After jumping or moving to another solar system, stale `beyonce`, `scanMgr`, and station `invbroker`
    //    monikers (including `beyonce.UpdateStateRequest`) must be rejected.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_000_144;
        }
    });
    session.solar_system_id = Some(30_000_144);
    assert_eq!(
        ctx.verify_bound_object(&session, inv_st_bid).unwrap_err().code,
        "BoundObjectOwnershipViolation"
    );
    assert_eq!(
        ctx.verify_bound_object(&session, jita_beyonce_bid)
            .unwrap_err()
            .code,
        "BoundObjectOwnershipViolation"
    );
    assert_eq!(
        ctx.verify_bound_object(&session, jita_scan_bid)
            .unwrap_err()
            .code,
        "BoundObjectOwnershipViolation"
    );
    let stale_usr = ctx
        .dispatch_rpc(
            &mut session,
            "beyonce",
            Some(jita_beyonce_bid),
            "UpdateStateRequest",
            &[],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(stale_usr.code, "BoundObjectOwnershipViolation");

    // 4. `userSvc.UserLogOffCharacter` must release all bound objects for the session.
    let perim_beyonce_bid = ctx.bind_object(&session, "beyonce", 30_000_144).unwrap();
    assert!(ctx.verify_bound_object(&session, perim_beyonce_bid).is_ok());
    ctx.dispatch_rpc(
        &mut session,
        "userSvc",
        None,
        "UserLogOffCharacter",
        &[],
        &kw,
        None,
    )
    .await
    .expect("UserLogOffCharacter");
    assert_eq!(
        ctx.verify_bound_object(&session, perim_beyonce_bid)
            .unwrap_err()
            .code,
        "InvalidBoundObject"
    );
}
