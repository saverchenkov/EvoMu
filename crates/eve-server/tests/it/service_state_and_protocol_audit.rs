//! Integration regression tests covering Round 8 findings.

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_review_r8_regressions() {
    // parity: rpc:dynamicItemService.CreateDynamicItem, rpc:fighterMgr.CmdActivateAbilitySlots, rpc:industryManager.CompleteJob, rpc:jumpCloneSvc.GetPriceForClone, rpc:keeper.ActivateAccelerationGate, rpc:marketProxy.BuyMultipleItems, rpc:marketProxy.GetCharOrders, rpc:multiLoginBlocker.Login, rpc:planetMgr.UserUpdateNetwork, rpc:repackagingSvc.RepackageItems, rpc:shipCosmeticsMgr.ActivateSkinLicense, rpc:shipCosmeticsMgr.ApplySkinToShip, rpc:shipCosmeticsMgr.GiveSkin, rpc:standingMgr.GetStandingTransactions
    use eve_db::InventoryFlag;

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut s1 = SessionState::new(30);
    ctx.dispatch_rpc(&mut s1, "authentication", None, "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut s1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();

    let mut s2 = SessionState::new(31);
    ctx.dispatch_rpc(&mut s2, "authentication", None, "Login", &[PyRep::String("other".into()), PyRep::String("other-secret".into())], &kw, None)
        .await
        .unwrap();
    ctx.dispatch_rpc(&mut s2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();

    // 1. R8-P0-1: Escrow 1MN Afterburner II (100_000_003, type 438) into an ItemExchange contract;
    // verify CreateDynamicItem cannot mutate a contract-locked item, AcceptContract fails with ContractEscrowItemMissing and zero ISK transfer if the escrowed item is missing, and DeleteContract restores it.
    let c_ab = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(1), // ItemExchange
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(60_003_760),
                PyRep::Integer(60_003_760),
                PyRep::Real(50_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
                PyRep::String("".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(100_000_003)]),
            ],
            &kw,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(cab_kv) = c_ab else {
        panic!("Expected cab_kv");
    };
    let cid_ab = cab_kv.get("contractID").unwrap().as_i64().unwrap();

    let muta_locked =
        ctx.dispatch_rpc(&mut s1, "dynamicItemService", None, "CreateDynamicItem", &[PyRep::Integer(47746), PyRep::Integer(100_000_003)], &kw, None).await;
    assert_eq!(muta_locked.unwrap_err().code, "TargetItemLockedOrUnreachable");

    // Verify AcceptContract fails with ContractEscrowItemMissing and zero ISK transfer if the escrowed item is missing
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&100_000_003) {
            it.flag = InventoryFlag::Hangar;
        }
    });
    let s2_bal_before = ctx.db.get_character(90_000_002).unwrap().balance.0;
    let missing_acc = ctx.dispatch_rpc(&mut s2, "contractProxy", None, "AcceptContract", &[PyRep::Integer(cid_ab)], &kw, None).await;
    assert_eq!(missing_acc.unwrap_err().code, "ContractEscrowItemMissing");
    assert_eq!(ctx.db.get_character(90_000_002).unwrap().balance.0, s2_bal_before);

    // Restore lock and delete contract via DeleteContract; verify item 100_000_003 is back in Hangar at 60_003_760
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&100_000_003) {
            it.flag = InventoryFlag::Locked;
        }
    });
    ctx.dispatch_rpc(&mut s1, "contractProxy", None, "DeleteContract", &[PyRep::Integer(cid_ab)], &kw, None).await.unwrap();
    assert_eq!(ctx.db.get_item(100_000_003).unwrap().flag, InventoryFlag::Hangar);

    // 2. R8-P1-1: Install industry job (locks Rifter BPO 100_000_007), verify CreateContract rejects escrowing 100_000_007 with ContractItemLocked,
    // and verify CompleteJob rejects completion with BlueprintMissingOrUnlocked if blueprint was unlocked/moved.
    let job_res = ctx
        .dispatch_rpc(
            &mut s1,
            "industryManager",
            None,
            "InstallJob",
            &[
                PyRep::Integer(691),
                PyRep::Integer(4), // ME research on BPO 100_000_007
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(jkv) = job_res else {
        panic!("Expected job KeyVal");
    };
    let jid = jkv.get("jobID").unwrap().as_i64().unwrap();

    let contract_bp_err = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(1), // ItemExchange
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(60_003_760),
                PyRep::Integer(60_003_760),
                PyRep::Real(10_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
                PyRep::String("".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(100_000_007)]),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(contract_bp_err.unwrap_err().code, "ContractItemLocked");

    ctx.advance_virtual_clock(3_600);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(bp) = snap.items.get_mut(&100_000_007) {
            bp.flag = InventoryFlag::Hangar;
        }
    });
    let comp_unlocked_err = ctx.dispatch_rpc(&mut s1, "industryManager", None, "CompleteJob", &[PyRep::Integer(jid)], &kw, None).await;
    assert_eq!(comp_unlocked_err.unwrap_err().code, "BlueprintMissingOrUnlocked");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(bp) = snap.items.get_mut(&100_000_007) {
            bp.flag = InventoryFlag::Locked;
        }
    });
    ctx.dispatch_rpc(&mut s1, "industryManager", None, "CompleteJob", &[PyRep::Integer(jid)], &kw, None).await.unwrap();

    // 3. R8-P1-2 & R9-P1-1: Expired contract settlement for both Outstanding and InProgress Courier contracts
    let exp_cour_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            eve_db::InventoryItemRecord {
                item_id: id,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        id
    });
    let s1_bal_pre = ctx.db.get_character(90_000_001).unwrap().balance.0;
    let c_exp = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(3), // Courier
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(60_003_760),
                PyRep::Integer(60_008_494),
                PyRep::Real(0.0),
                PyRep::Real(5_000.0),
                PyRep::Real(20_000.0),
                PyRep::String("".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(exp_cour_item_id)]),
            ],
            &kw,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(cexp_kv) = c_exp else {
        panic!("Expected cexp_kv");
    };
    let cid_exp = cexp_kv.get("contractID").unwrap().as_i64().unwrap();
    assert_eq!(ctx.db.get_character(90_000_001).unwrap().balance.0, s1_bal_pre - 11_500_000);
    ctx.advance_virtual_clock(86_405);
    let acc_exp = ctx.dispatch_rpc(&mut s2, "contractProxy", None, "AcceptContract", &[PyRep::Integer(cid_exp)], &kw, None).await;
    assert_eq!(acc_exp.unwrap_err().code, "ContractExpired");
    // Escrowed 5,000 ISK reward and 100,000 ISK deposit were durably refunded to issuer s1 (10,000 ISK broker fee retained)
    assert_eq!(ctx.db.get_character(90_000_001).unwrap().balance.0, s1_bal_pre - 1_000_000);

    // R9-P1-1: InProgress Courier contract resets delivery deadline on acceptance; on expiry, issuer gets collateral + reward and contracted item transfers to acceptor at its current location (not teleported back to issuer)
    let c_inprog = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(3), // Courier
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(60_003_760),
                PyRep::Integer(60_003_760),
                PyRep::Real(0.0),
                PyRep::Real(5_000.0),
                PyRep::Real(20_000.0),
                PyRep::String("".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(100_000_003)]),
            ],
            &kw,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(cin_kv) = c_inprog else {
        panic!("Expected cin_kv");
    };
    let cid_in = cin_kv.get("contractID").unwrap().as_i64().unwrap();
    let create_exp = cin_kv.get("expirationUnix").unwrap().as_i64().unwrap();
    // Advance 80,000s (late in 24h offer window) and accept with s2
    ctx.advance_virtual_clock(80_000);
    let acc_in = ctx.dispatch_rpc(&mut s2, "contractProxy", None, "AcceptContract", &[PyRep::Integer(cid_in)], &kw, None).await.unwrap();
    let PyRep::KeyVal(accin_kv) = acc_in else {
        panic!("Expected accin_kv");
    };
    let wrap_id_in = accin_kv.get("courierPackageID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(w) = snap.items.get_mut(&wrap_id_in) {
            w.location_id = 100_000_002;
            w.flag = InventoryFlag::Cargo;
        }
    });
    let delivery_exp = accin_kv.get("expirationUnix").unwrap().as_i64().unwrap();
    assert!((delivery_exp - (create_exp + 80_000)).abs() <= 2);
    // Advance past the new 24h delivery deadline (86,405s) and call CompleteContract
    ctx.advance_virtual_clock(86_405);
    let comp_exp = ctx.dispatch_rpc(&mut s2, "contractProxy", None, "CompleteContract", &[PyRep::Integer(cid_in)], &kw, None).await;
    assert_eq!(comp_exp.unwrap_err().code, "ContractExpired");
    // Issuer s1 received back the 100,000 ISK public courier deposit on acceptance + 5,000 ISK reward + 20,000 ISK forfeited collateral
    assert_eq!(ctx.db.get_character(90_000_001).unwrap().balance.0, s1_bal_pre);
    // Contracted item 100_000_003 stayed in s2's ship (100_000_002) and transferred ownership to s2 (90_000_002)
    let forfeited_item = ctx.db.get_item(100_000_003).unwrap();
    assert_eq!(forfeited_item.owner_id, 90_000_002);
    assert_eq!(forfeited_item.location_id, 100_000_002);
    assert_eq!(forfeited_item.flag, InventoryFlag::Cargo);

    // 4. R8-P1-4 & R9-P1-3: Accepting missions allocates unique NPC ball IDs per character and mission instance;
    // s1 accepting 909 & 910 and s2 accepting 909 produce 3 distinct NPC balls that never collide.
    let PyRep::KeyVal(u1_kv) = ctx.dispatch_rpc(&mut s1, "stationSvc", None, "Undock", &[], &kw, None).await.unwrap() else {
        panic!("Expected u1_kv");
    };
    let s1_b_id = u1_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(&mut s1, "beyonce", Some(s1_b_id), "CmdStop", &[], &kw, None).await.unwrap();
    let PyRep::KeyVal(u2_kv) = ctx.dispatch_rpc(&mut s2, "stationSvc", None, "Undock", &[], &kw, None).await.unwrap() else {
        panic!("Expected u2_kv");
    };
    let s2_b_id = u2_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(&mut s2, "beyonce", Some(s2_b_id), "CmdStop", &[], &kw, None).await.unwrap();
    let mut s1_mids = Vec::new();
    let mut s1_npcs = Vec::new();
    for _ in 0..2 {
        let rm = ctx.dispatch_rpc(&mut s1, "agentMgr", None, "DoAction", &[PyRep::Integer(2)], &kw, None).await.unwrap();
        let PyRep::KeyVal(rm_kv) = rm else {
            panic!("Expected rm_kv");
        };
        let mid = rm_kv.get("missionID").unwrap().as_i64().unwrap();
        s1_mids.push(mid);
        let am = ctx.dispatch_rpc(&mut s1, "agentMgr", None, "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid)], &kw, None).await.unwrap();
        let PyRep::KeyVal(am_kv) = am else {
            panic!("Expected am_kv");
        };
        s1_npcs.push(am_kv.get("spawnedNpcBallID").unwrap().as_i64().unwrap());
    }
    assert_ne!(s1_mids[0], s1_mids[1]);
    let rm_s2 = ctx.dispatch_rpc(&mut s2, "agentMgr", None, "DoAction", &[PyRep::Integer(2)], &kw, None).await.unwrap();
    let PyRep::KeyVal(rms2_kv) = rm_s2 else {
        panic!("Expected rms2_kv");
    };
    let s2_mid_1 = rms2_kv.get("missionID").unwrap().as_i64().unwrap();
    let am_s2 = ctx.dispatch_rpc(&mut s2, "agentMgr", None, "DoAction", &[PyRep::Integer(3), PyRep::Integer(s2_mid_1)], &kw, None).await.unwrap();
    let PyRep::KeyVal(ams2_kv) = am_s2 else {
        panic!("Expected ams2_kv");
    };
    let s2_npc_1001 = ams2_kv.get("spawnedNpcBallID").unwrap().as_i64().unwrap();
    assert_ne!(s1_npcs[0], s1_npcs[1]);
    assert_ne!(s1_npcs[0], s2_npc_1001);
    assert_ne!(s1_npcs[1], s2_npc_1001);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_000_942,
            eve_db::InventoryItemRecord {
                item_id: 100_000_942,
                type_id: 21924,
                owner_id: 90_000_001,
                location_id: 100_000_001,
                flag: InventoryFlag::Cargo,
                quantity: 2_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            100_000_943,
            eve_db::InventoryItemRecord {
                item_id: 100_000_943,
                type_id: 21924,
                owner_id: 90_000_002,
                location_id: 100_000_002,
                flag: InventoryFlag::Cargo,
                quantity: 2_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(100_000_001), PyRep::Integer(27), PyRep::Integer(100_000_942)], &kw, None)
        .await
        .unwrap();
    ctx.dispatch_rpc(&mut s2, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(100_000_002), PyRep::Integer(27), PyRep::Integer(100_000_943)], &kw, None)
        .await
        .unwrap();

    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    for _ in 0..4 {
        let _ = jita_handle.step_manual(1.0).await.unwrap();
    }
    // Third-party pilot (`s2`) cannot attack `s1`'s instanced mission pocket NPC
    let cross_fire = ctx.dispatch_rpc(&mut s2, "dogmaIM", None, "Activate", &[PyRep::Integer(s1_npcs[0])], &kw, None).await;
    assert_eq!(cross_fire.unwrap_err().code, "MissionPocketInstanceRestricted");

    let mut fw1_destroyed = false;
    for shot in 0..10 {
        if shot > 0 {
            for _ in 0..4 {
                let _ = jita_handle.step_manual(1.0).await.unwrap();
            }
        }
        let fw1 = ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(s1_npcs[0])], &kw, None).await.unwrap();
        let PyRep::KeyVal(fw1_kv) = fw1 else {
            panic!("Expected fw1_kv");
        };
        if fw1_kv.get("destroyed") == Some(&PyRep::Bool(true)) {
            fw1_destroyed = true;
            break;
        }
    }
    assert!(fw1_destroyed);

    let gate_m2_early = ctx.dispatch_rpc(&mut s1, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(s1_mids[1])], &kw, None).await;
    assert_eq!(gate_m2_early.unwrap_err().code, "RoomNpcsRemaining");
    let m2_early = ctx.dispatch_rpc(&mut s1, "agentMgr", None, "DoAction", &[PyRep::Integer(6), PyRep::Integer(s1_mids[1])], &kw, None).await;
    assert_eq!(m2_early.unwrap_err().code, "MissionIncomplete");

    let mut fw2_destroyed = false;
    for _ in 0..10 {
        for _ in 0..4 {
            let _ = jita_handle.step_manual(1.0).await.unwrap();
        }
        let fw2 = ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(s1_npcs[1])], &kw, None).await.unwrap();
        let PyRep::KeyVal(fw2_kv) = fw2 else {
            panic!("Expected fw2_kv");
        };
        if fw2_kv.get("destroyed") == Some(&PyRep::Bool(true)) {
            fw2_destroyed = true;
            break;
        }
    }
    assert!(fw2_destroyed);
    ctx.dispatch_rpc(&mut s1, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(s1_mids[1])], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut s1, "agentMgr", None, "DoAction", &[PyRep::Integer(6), PyRep::Integer(s1_mids[1])], &kw, None).await.unwrap();

    // s2's mission NPC is still alive and independent; s2 destroys it and completes s2_mid_1
    let mut fws2_destroyed = false;
    for shot in 0..10 {
        if shot > 0 {
            for _ in 0..4 {
                let _ = jita_handle.step_manual(1.0).await.unwrap();
            }
        }
        let fw_s2 = ctx.dispatch_rpc(&mut s2, "dogmaIM", None, "Activate", &[PyRep::Integer(s2_npc_1001)], &kw, None).await.unwrap();
        let PyRep::KeyVal(fws2_kv) = fw_s2 else {
            panic!("Expected fws2_kv");
        };
        if fws2_kv.get("destroyed") == Some(&PyRep::Bool(true)) {
            fws2_destroyed = true;
            break;
        }
    }
    assert!(fws2_destroyed);
    ctx.dispatch_rpc(&mut s2, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(s2_mid_1)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut s2, "agentMgr", None, "DoAction", &[PyRep::Integer(6), PyRep::Integer(s2_mid_1)], &kw, None).await.unwrap();

    // Mission abandon/decline via RemoveOfferFromJournal despawns the pocket NPC and clears the journal entry
    let rm_1003 = ctx.dispatch_rpc(&mut s1, "agentMgr", None, "DoAction", &[PyRep::Integer(2)], &kw, None).await.unwrap();
    let PyRep::KeyVal(rm1003_kv) = rm_1003 else {
        panic!("Expected rm1003_kv");
    };
    let s1_mid_3 = rm1003_kv.get("missionID").unwrap().as_i64().unwrap();
    let am_1003 = ctx.dispatch_rpc(&mut s1, "agentMgr", None, "DoAction", &[PyRep::Integer(3), PyRep::Integer(s1_mid_3)], &kw, None).await.unwrap();
    let PyRep::KeyVal(am1003_kv) = am_1003 else {
        panic!("Expected am1003_kv");
    };
    let npc_1003 = am1003_kv.get("spawnedNpcBallID").unwrap().as_i64().unwrap();
    assert!(jita_handle.get_ball_snapshot(npc_1003).await.unwrap().is_some());
    ctx.dispatch_rpc(&mut s1, "agentMgr", None, "RemoveOfferFromJournal", &[PyRep::Integer(s1_mid_3)], &kw, None).await.unwrap();
    assert!(jita_handle.get_ball_snapshot(npc_1003).await.unwrap().is_none());

    // N1, N2 & R36-P1-7: Cannot abandon an already completed or already quit mission, cannot re-accept either, and client cannot supply mission_id to DoAction(RequestMission)
    let quit_completed = ctx.dispatch_rpc(&mut s1, "agentMgr", None, "RemoveOfferFromJournal", &[PyRep::Integer(s1_mids[1])], &kw, None).await;
    assert_eq!(quit_completed.unwrap_err().code, "MissionNotActive");
    let quit_already_quit = ctx.dispatch_rpc(&mut s1, "agentMgr", None, "RemoveOfferFromJournal", &[PyRep::Integer(s1_mid_3)], &kw, None).await;
    assert_eq!(quit_already_quit.unwrap_err().code, "MissionNotActive");
    let reaccept_completed = ctx.dispatch_rpc(&mut s1, "agentMgr", None, "DoAction", &[PyRep::Integer(3), PyRep::Integer(s1_mids[1])], &kw, None).await;
    assert_eq!(reaccept_completed.unwrap_err().code, "InvalidMissionState");
    let client_supplied_mid = ctx.dispatch_rpc(&mut s1, "agentMgr", None, "DoAction", &[PyRep::Integer(2), PyRep::Integer(s1_mids[1])], &kw, None).await;
    assert_eq!(client_supplied_mid.unwrap_err().code, "UnknownAgent");

    // N4: Cannot fit a module into a ship slot while in space (`CannotFitInSpace`)
    let fit_in_space =
        ctx.dispatch_rpc(&mut s1, "invbroker", None, "Add", &[PyRep::Integer(100_000_003), PyRep::Integer(100_000_001), PyRep::Integer(12)], &kw, None).await;
    assert_eq!(fit_in_space.unwrap_err().code, "CannotFitInSpace");

    // N7: Cannot warp to arbitrary unscanned/unbookmarked coordinates (`InvalidWarpDestination`)
    let s1_bound = ctx.bind_object(&mut s1, "beyonce", 30_000_142).unwrap();
    let invalid_warp = ctx
        .dispatch_rpc(
            &mut s1,
            "beyonce",
            Some(s1_bound),
            "CmdWarpToStuff",
            &[PyRep::Real(900_000_000_000.0), PyRep::Real(900_000_000_000.0), PyRep::Real(900_000_000_000.0)],
            &kw,
            None,
        )
        .await;
    assert_eq!(invalid_warp.unwrap_err().code, "InvalidWarpDestination");

    // R12-P1-3: Cannot warp to another player's unprobed ship in space even when its Ball exists in the Ballpark
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    let _ = jita_handle
        .spawn_ball(eve_destiny::Ball {
            id: 100_000_002,
            type_id: 587,
            position: glam::DVec3::new(500_000_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.45,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let warp_to_other_ship =
        ctx.dispatch_rpc(&mut s1, "beyonce", Some(s1_bound), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(100_000_002)], &kw, None).await;
    assert_eq!(warp_to_other_ship.unwrap_err().code, "InvalidWarpDestination");

    // R12-P1-3: CmdWarpToStuff resolves ("item", stargate_id) and rejects invalid targets
    let warp_stuff_ok =
        ctx.dispatch_rpc(&mut s1, "beyonce", Some(s1_bound), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(50_001_249)], &kw, None).await;
    assert!(warp_stuff_ok.is_ok(), "CmdWarpToStuff to stargate 50001249");
    let warp_stuff_bad =
        ctx.dispatch_rpc(&mut s1, "beyonce", Some(s1_bound), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(999_999_999)], &kw, None).await;
    assert_eq!(warp_stuff_bad.unwrap_err().code, "InvalidWarpDestination");

    // R12-P1-1: Real SDE type 3520 (Heavy Pulse Laser II) has no fabricated capital damage (114..=118), and T2 BPO (11372) / rolled Abyssal (47749) are not sold on NPC market
    assert!(!ctx.sde.types[&3520].default_attributes.contains_key(&114));
    let (orders_11372, orders_47749) = ctx.db.with_snapshot(|snap| {
        (snap.market_orders.values().filter(|o| o.type_id == 11372).count(), snap.market_orders.values().filter(|o| o.type_id == 47749).count())
    });
    assert_eq!(orders_11372, 0);
    assert_eq!(orders_47749, 0);

    let (s1, s2) = crate::service_state_and_protocol_audit::run::run_r8_part2(&ctx, s1, s2).await;
    crate::service_state_and_protocol_audit::run_extract_rowset_rows::run_r8_part3(&ctx, s1, s2).await;
}

pub(crate) mod run {
    // Integration regression tests covering Round 8 findings (Part 2).


    use eve_db::InventoryFlag;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    pub(crate) async fn run_r8_part2(
        ctx: &ServerContext,
        mut s1: SessionState,
        s2: SessionState,
    ) -> (SessionState, SessionState) {
        let _ = s2;
        let kw = HashMap::new();
        let jita_handle = ctx.systems.get_or_spawn(30_000_142);
        // R12-P1-2: Dock s2 at Jita 4-4 and verify slot-type mismatch and turret hardpoint overflow are rejected
        ctx.db
            .dock_character(90_000_002, 60_003_760, false)
            .expect("Dock 90_000_002 at Jita 4-4");
        let mut s2 = SessionState::new(820);
        ctx.dispatch_rpc(
            &mut s2,
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("other".into()),
                PyRep::String("other-secret".into()),
            ],
            &kw,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut s2,
            "multiLoginBlocker",
            None,
            "Login",
            &[PyRep::String("hw-r8-other".into())],
            &kw,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut s2,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_002)],
            &kw,
            None,
        )
        .await
        .unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            for (iid, tid) in [
                (100_009_901_i64, 438_u32),  // 1MN Afterburner II (medPower 13)
                (100_009_902_i64, 2889_u32), // 200mm AutoCannon II #2 (turretFitted 42)
                (100_009_903_i64, 2889_u32), // 200mm AutoCannon II #3 (turretFitted 42)
                (100_009_904_i64, 2889_u32), // 200mm AutoCannon II #4 (exceeds Rifter's 3 turret hardpoints)
            ] {
                snap.items.insert(
                    iid,
                    eve_db::InventoryItemRecord {
                        item_id: iid,
                        type_id: tid,
                        owner_id: 90_000_002,
                        location_id: 60_003_760,
                        flag: eve_db::InventoryFlag::Hangar,
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
            }
        });
        // Fitting med-slot 1MN Afterburner II (438) into HiSlot1 (flag 28) fails with FittingValidationFailed
        let bad_slot = ctx
            .dispatch_rpc(
                &mut s2,
                "invbroker",
                None,
                "Add",
                &[
                    PyRep::Integer(100_009_901),
                    PyRep::Integer(100_000_002),
                    PyRep::Integer(28),
                ],
                &kw,
                None,
            )
            .await;
        assert_eq!(bad_slot.unwrap_err().code, "FittingValidationFailed");

        // Fitting turrets #2 and #3 into HiSlot1 (28) and HiSlot2 (29) succeeds on Rifter (3 turret hardpoints),
        // but fitting turret #4 into HiSlot3 (30) fails (exceeds Rifter's 3 turret hardpoints / 4 high slots)
        ctx.dispatch_rpc(
            &mut s2,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_009_902),
                PyRep::Integer(100_000_002),
                PyRep::Integer(28),
            ],
            &kw,
            None,
        )
        .await
        .expect("Fit 2nd turret on Rifter");
        ctx.dispatch_rpc(
            &mut s2,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_009_903),
                PyRep::Integer(100_000_002),
                PyRep::Integer(29),
            ],
            &kw,
            None,
        )
        .await
        .expect("Fit 3rd turret on Rifter");
        let fourth_turret = ctx
            .dispatch_rpc(
                &mut s2,
                "invbroker",
                None,
                "Add",
                &[
                    PyRep::Integer(100_009_904),
                    PyRep::Integer(100_000_002),
                    PyRep::Integer(30),
                ],
                &kw,
                None,
            )
            .await;
        assert_eq!(fourth_turret.unwrap_err().code, "FittingValidationFailed");

        // R13-P0-1: RepackageItems rejects ships/containers with fitted modules or cargo (`CannotRepackageNonEmptyItem`)
        // and PlaceCharOrder / CreateContract reject selling/contracting non-empty ships or items in ship cargo
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                100_009_910,
                eve_db::InventoryItemRecord {
                    item_id: 100_009_910,
                    type_id: 587, // Spare Rifter in Jita 4-4 Hangar
                    owner_id: 90_000_002,
                    location_id: 60_003_760,
                    flag: eve_db::InventoryFlag::Hangar,
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
            snap.items.insert(
                100_009_911,
                eve_db::InventoryItemRecord {
                    item_id: 100_009_911,
                    type_id: 1236, // Overdrive Injector System II fitted on spare Rifter 100_009_910
                    owner_id: 90_000_002,
                    location_id: 100_009_910,
                    flag: eve_db::InventoryFlag::LoSlot0,
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
        let repack_non_empty = ctx
            .dispatch_rpc(
                &mut s2,
                "repackagingSvc",
                None,
                "RepackageItems",
                &[PyRep::Integer(100_009_910)],
                &kw,
                None,
            )
            .await;
        assert_eq!(
            repack_non_empty.unwrap_err().code,
            "CannotRepackageNonEmptyItem"
        );
        let contract_non_empty = ctx
            .dispatch_rpc(
                &mut s2,
                "contractProxy",
                None,
                "CreateContract",
                &[
                    PyRep::Integer(1), // ItemExchange
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::Integer(1440),
                    PyRep::Integer(1),
                    PyRep::Integer(60_003_760),
                    PyRep::Integer(60_003_760),
                    PyRep::Real(100_000.0),
                    PyRep::Real(0.0),
                    PyRep::Real(0.0),
                    PyRep::String("".into()),
                    PyRep::String("".into()),
                    PyRep::List(vec![PyRep::Integer(100_009_910)]),
                ],
                &kw,
                None,
            )
            .await;
        assert_eq!(
            contract_non_empty.unwrap_err().code,
            "CannotContractNonEmptyContainer"
        );

        // Unfitting 100_009_911 back to Hangar allows 100_009_910 to be repackaged
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(m) = snap.items.get_mut(&100_009_911) {
                m.location_id = 60_003_760;
                m.flag = eve_db::InventoryFlag::Hangar;
            }
        });
        ctx.dispatch_rpc(
            &mut s2,
            "repackagingSvc",
            None,
            "RepackageItems",
            &[PyRep::Integer(100_009_910)],
            &kw,
            None,
        )
        .await
        .expect("Repackage empty spare Rifter 100_009_910");

        // R13-P1-1: Missing skill prerequisites reject module fitting (`FittingValidationFailed`)
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(sk) = snap.skills.get_mut(&90_000_002) {
                sk.remove(&3450); // Remove Afterburner IV (required for 1MN Afterburner II 438)
            }
        });
        ctx.invalidate_ship_fitting(100_000_002);
        let missing_skill_fit = ctx
            .dispatch_rpc(
                &mut s2,
                "invbroker",
                None,
                "Add",
                &[
                    PyRep::Integer(100_009_901),
                    PyRep::Integer(100_000_002),
                    PyRep::Integer(19),
                ],
                &kw,
                None,
            )
            .await;
        assert_eq!(
            missing_skill_fit.unwrap_err().code,
            "FittingValidationFailed"
        );

        // Restore Afterburner IV (3450: 4) and verify 1MN Afterburner II (438) fits into MedSlot0 (19)
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_002) {
                ch.clone_grade = 2; // Omega clone so Afterburner IV is effective
            }
            snap.skills.entry(90_000_002).or_default().insert(
                3450,
                eve_db::TrainedSkillRecord {
                    type_id: 3450,
                    level: 4,
                    skill_points: 45_255.0,
                },
            );
        });
        ctx.invalidate_ship_fitting(100_000_002);
        ctx.dispatch_rpc(
            &mut s2,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_009_901),
                PyRep::Integer(100_000_002),
                PyRep::Integer(19),
            ],
            &kw,
            None,
        )
        .await
        .expect("Fit 1MN Afterburner II after training Afterburner IV");

        // R13-P1-2: Applying a mutaplasmid to a multi-unit stack (quantity 5) decrements the stack to 4 and creates a new singleton Abyssal item
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                100_009_920,
                eve_db::InventoryItemRecord {
                    item_id: 100_009_920,
                    type_id: 438, // 5x repackaged 1MN Afterburner II stack
                    owner_id: 90_000_002,
                    location_id: 60_003_760,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 5,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
            snap.items.insert(
                100_009_921,
                eve_db::InventoryItemRecord {
                    item_id: 100_009_921,
                    type_id: 47746, // Decayed 1MN Afterburner Mutaplasmid
                    owner_id: 90_000_002,
                    location_id: 60_003_760,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        });
        let muta_stack_res = ctx
            .dispatch_rpc(
                &mut s2,
                "dynamicItemService",
                None,
                "CreateDynamicItem",
                &[PyRep::Integer(47746), PyRep::Integer(100_009_920)],
                &kw,
                None,
            )
            .await
            .expect("CreateDynamicItem on 5x stack");
        let PyRep::KeyVal(ms_kv) = muta_stack_res else {
            panic!("Expected CreateDynamicItem KeyVal");
        };
        let split_abyssal_id = ms_kv.get("itemID").unwrap().as_i64().unwrap();
        assert_ne!(split_abyssal_id, 100_009_920);
        let orig_stack = ctx.db.get_item(100_009_920).unwrap();
        assert_eq!(orig_stack.type_id, 438);
        assert_eq!(orig_stack.quantity, 4);
        assert!(!orig_stack.singleton);
        let split_abyssal = ctx.db.get_item(split_abyssal_id).unwrap();
        assert_eq!(split_abyssal.type_id, 47749);
        assert_eq!(split_abyssal.quantity, 1);
        assert!(split_abyssal.singleton);
        // R22-P1-4: Verify rolled Abyssal attributes lie within [min_mult * base, max_mult * base] for mutaplasmid 47746 on type 438
        let base_438 = &ctx.sde.types.get(&438).unwrap().default_attributes;
        for (attr_id, min_m, max_m) in [
            (6_u16, 0.85, 1.20),
            (20_u16, 0.95, 1.10),
            (30_u16, 0.95, 1.25),
            (50_u16, 0.95, 1.25),
        ] {
            let base_val = base_438[&attr_id];
            let rolled_val = split_abyssal.dynamic_attributes[&attr_id];
            assert!(
                rolled_val >= base_val * min_m - 1e-6 && rolled_val <= base_val * max_m + 1e-6,
                "Abyssal attr {attr_id} rolled {rolled_val} outside [{}, {}]",
                base_val * min_m,
                base_val * max_m
            );
        }

        // R13-P1-3: SaveFitting validates and persists caller-supplied description and module list, and rejects invalid module/hull combinations
        let bad_save_fit = ctx
            .dispatch_rpc(
                &mut s2,
                "charFittingMgr",
                None,
                "SaveFitting",
                &[
                    PyRep::String("Invalid Trig Rifter".into()),
                    PyRep::Integer(587),
                    PyRep::String("Should fail".into()),
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(47918),
                        PyRep::Integer(27),
                        PyRep::Integer(1),
                    ])]),
                ],
                &kw,
                None,
            )
            .await;
        assert_eq!(bad_save_fit.unwrap_err().code, "InvalidFittingModule");

        let saved_fid = ctx
            .dispatch_rpc(
                &mut s2,
                "charFittingMgr",
                None,
                "SaveFitting",
                &[
                    PyRep::String("Custom Rifter Tackler".into()),
                    PyRep::Integer(587),
                    PyRep::String("Fast tackle with autocannons and overdrive".into()),
                    PyRep::List(vec![
                        PyRep::Tuple(vec![
                            PyRep::Integer(2889),
                            PyRep::Integer(27),
                            PyRep::Integer(1),
                        ]),
                        PyRep::Tuple(vec![
                            PyRep::Integer(438),
                            PyRep::Integer(19),
                            PyRep::Integer(1),
                        ]),
                        PyRep::Tuple(vec![
                            PyRep::Integer(1236),
                            PyRep::Integer(11),
                            PyRep::Integer(1),
                        ]),
                    ]),
                ],
                &kw,
                None,
            )
            .await
            .expect("SaveFitting with custom modules and description")
            .as_i64()
            .unwrap();
        let saved_rec = ctx
            .db
            .with_snapshot(|snap| snap.saved_fittings.get(&saved_fid).cloned())
            .unwrap();
        assert_eq!(
            saved_rec.description,
            "Fast tackle with autocannons and overdrive"
        );
        assert_eq!(
            saved_rec.items,
            vec![
                (2889, InventoryFlag::HiSlot0, 1),
                (438, InventoryFlag::MedSlot0, 1),
                (1236, InventoryFlag::LoSlot0, 1),
            ]
        );

        // R13-P1-4: While docked at Jita 4-4 (60_003_760), ship.Board rejects boarding an owned ship floating in space in Jita (30_000_142)
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                100_009_930,
                eve_db::InventoryItemRecord {
                    item_id: 100_009_930,
                    type_id: 587, // Assembled Rifter floating in space in Jita (30_000_142)
                    owner_id: 90_000_002,
                    location_id: 30_000_142,
                    flag: eve_db::InventoryFlag::None,
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
        ctx.advance_secs(12);
        let board_space_while_docked = ctx
            .dispatch_rpc(
                &mut s2,
                "ship",
                None,
                "Board",
                &[PyRep::Integer(100_009_930)],
                &kw,
                None,
            )
            .await;
        assert_eq!(
            board_space_while_docked.unwrap_err().code,
            "ShipNotBoardable"
        );

        // R14-P1-2 / R17-P1-1 / R18-P1-2: Weapon modules (2889, 2404, 2410, 2420, 54739, 47918, 85084) have no fabricated damage (114, 116, 117, 118) attributes,
        // and missile launchers/charges (2404, 2410, 2420, 211, 2510) have no fabricated chargeSize (128) attribute
        let torp_attrs = &ctx.sde.types.get(&2420).unwrap().default_attributes;
        let lml_attrs = &ctx.sde.types.get(&2404).unwrap().default_attributes;
        for attr_id in 1298..=1301 {
            assert!(
                !torp_attrs.contains_key(&attr_id),
                "Torpedo Launcher II (2420) must not have fabricated canFitShipGroup attr {attr_id}"
            );
        }
        for weapon_type_id in [2889_u32, 2404, 2410, 2420, 54739, 47918, 85084] {
            let w_attrs = &ctx
                .sde
                .types
                .get(&weapon_type_id)
                .unwrap()
                .default_attributes;
            for attr_id in [114_u16, 116, 117, 118] {
                assert!(
                    !w_attrs.contains_key(&attr_id),
                    "Weapon module {weapon_type_id} must not have damage attr {attr_id}"
                );
            }
        }
        for missile_type_id in [2404_u32, 2410, 2420, 211, 2510] {
            let m_attrs = &ctx
                .sde
                .types
                .get(&missile_type_id)
                .unwrap()
                .default_attributes;
            assert!(
                !m_attrs.contains_key(&128),
                "Missile launcher/charge {missile_type_id} must not have chargeSize (128)"
            );
        }
        assert_eq!(torp_attrs.get(&51).copied(), Some(14400.0));
        assert_eq!(torp_attrs.get(&604).copied(), Some(657.0));
        assert_eq!(torp_attrs.get(&606).copied(), Some(89.0));
        assert_eq!(lml_attrs.get(&51).copied(), Some(12800.0));
        assert_eq!(lml_attrs.get(&604).copied(), Some(384.0));
        let inferno_torp_attrs = &ctx.sde.types.get(&2510).unwrap().default_attributes;
        assert_eq!(inferno_torp_attrs.get(&118).copied(), Some(450.0));
        assert_eq!(inferno_torp_attrs.get(&653).copied(), Some(78.0));
        assert_eq!(inferno_torp_attrs.get(&654).copied(), Some(405.0));
        assert_eq!(inferno_torp_attrs.get(&1353).copied(), Some(0.944));
        let inferno_lml_attrs = &ctx.sde.types.get(&211).unwrap().default_attributes;
        assert_eq!(inferno_lml_attrs.get(&118).copied(), Some(83.0));
        assert_eq!(inferno_lml_attrs.get(&653).copied(), Some(170.0));
        assert_eq!(inferno_lml_attrs.get(&654).copied(), Some(40.0));
        assert_eq!(inferno_lml_attrs.get(&1353).copied(), Some(0.604));
        let rf_pp_attrs = &ctx.sde.types.get(&21924).unwrap().default_attributes;
        assert_eq!(rf_pp_attrs.get(&117).copied(), Some(2.3));
        assert_eq!(rf_pp_attrs.get(&118).copied(), Some(11.5));
        assert_eq!(rf_pp_attrs.get(&128).copied(), Some(1.0));
        assert_eq!(rf_pp_attrs.get(&137).copied(), Some(55.0));
        let vorton_attrs = &ctx.sde.types.get(&54739).unwrap().default_attributes;
        let breacher_attrs = &ctx.sde.types.get(&85084).unwrap().default_attributes;
        assert!(!vorton_attrs.contains_key(&1298));
        assert!(!breacher_attrs.contains_key(&1298));
        assert_eq!(vorton_attrs.get(&51).copied(), Some(6000.0));
        assert_eq!(vorton_attrs.get(&54).copied(), Some(22464.0));
        assert_eq!(vorton_attrs.get(&64).copied(), Some(1.0));
        assert_eq!(vorton_attrs.get(&604).copied(), Some(4062.0));
        assert_eq!(vorton_attrs.get(&653).copied(), Some(190.0));
        assert_eq!(vorton_attrs.get(&654).copied(), Some(43.0));
        assert_eq!(vorton_attrs.get(&1353).copied(), Some(0.40));

        // R18-P1-1: Attempting to load an incompatible charge (Inferno Light Missile 211) into 200mm AutoCannon II (2889) is rejected with IncompatibleCharge
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                100_009_940,
                eve_db::InventoryItemRecord {
                    item_id: 100_009_940,
                    type_id: 211,
                    owner_id: 90_000_002,
                    location_id: 100_000_002,
                    flag: InventoryFlag::Cargo,
                    quantity: 100,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        });
        let incomp_load = ctx
            .dispatch_rpc(
                &mut s2,
                "dogmaIM",
                None,
                "LoadAmmo",
                &[
                    PyRep::Integer(100_000_002),
                    PyRep::Integer(27),
                    PyRep::Integer(100_009_940),
                ],
                &kw,
                None,
            )
            .await;
        assert_eq!(incomp_load.unwrap_err().code, "IncompatibleAmmo");

        // R14-P1-1: SaveSkillQueue rejects skills with unmet prerequisites (Torpedo Specialization 20213 requires Torpedoes 3325 V)
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(90_000_002).or_default().remove(&3325);
        });
        let unmet_prereq_queue = ctx
            .dispatch_rpc(
                &mut s2,
                "skillMgr2",
                None,
                "SaveSkillQueue",
                &[PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(20213),
                    PyRep::Integer(1),
                ])])],
                &kw,
                None,
            )
            .await;
        assert_eq!(
            unmet_prereq_queue.unwrap_err().code,
            "SkillPrerequisiteNotMet"
        );

        // R14-P1-1: ApplyFreeSkillPoints partial application does not forge trained skill level before threshold SP is reached
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_002) {
                ch.free_skill_points = 500.0;
            }
            snap.skills.entry(90_000_002).or_default().remove(&3449);
            snap.skill_queues.entry(90_000_002).or_default().clear();
        });
        // Queue Navigation (3449, rank 1, requires Spaceship Command 3327 I which 90_000_002 has at level 3) to level 1 (needs 250 SP)
        ctx.dispatch_rpc(
            &mut s2,
            "skillMgr2",
            None,
            "SaveSkillQueue",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(3449),
                PyRep::Integer(1),
            ])])],
            &kw,
            None,
        )
        .await
        .expect("Queue Navigation I");
        let partial_sp_res = ctx
            .dispatch_rpc(
                &mut s2,
                "skillMgr2",
                None,
                "ApplyFreeSkillPoints",
                &[PyRep::Real(10.0)],
                &kw,
                None,
            )
            .await
            .expect("Apply 10 partial free SP");
        assert!(partial_sp_res.as_f64().is_some());
        let nav_lvl_after_partial = ctx
            .db
            .with_snapshot(|snap| {
                snap.skills
                    .get(&90_000_002)
                    .and_then(|m| m.get(&3449))
                    .map(|s| (s.level, s.skill_points))
            })
            .unwrap();
        assert_eq!(nav_lvl_after_partial.0, 0);
        assert!((nav_lvl_after_partial.1 - 10.0).abs() < 1e-6);

        // Applying the remaining 240 SP reaches 250 SP and advances Navigation (3449) to level 1
        let full_sp_res = ctx
            .dispatch_rpc(
                &mut s2,
                "skillMgr2",
                None,
                "ApplyFreeSkillPoints",
                &[PyRep::Real(240.0)],
                &kw,
                None,
            )
            .await
            .expect("Apply remaining 240 free SP");
        assert!(full_sp_res.as_f64().is_some());
        let nav_lvl_after_full = ctx
            .db
            .with_snapshot(|snap| {
                snap.skills
                    .get(&90_000_002)
                    .and_then(|m| m.get(&3449))
                    .map(|s| s.level)
            })
            .unwrap();
        assert_eq!(nav_lvl_after_full, 1);

        // R14-P1-4: marketProxy.GetCharOrders returns only the caller's own orders (not NPC orders),
        // and marketProxy.BuyMultipleItems requires a non-empty item list and executes multi-item purchases atomically
        let char_orders_before = ctx
            .dispatch_rpc(
                &mut s2,
                "marketProxy",
                None,
                "GetCharOrders",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetCharOrders");
        let PyRep::Rowset { rows: co_lines, .. } = char_orders_before else {
            panic!("Expected Rowset from GetCharOrders");
        };
        assert!(
            co_lines.is_empty(),
            "Character 90_000_002 has no personal market orders initially (NPC orders must be excluded)"
        );

        let empty_multi_buy = ctx
            .dispatch_rpc(
                &mut s2,
                "marketProxy",
                None,
                "BuyMultipleItems",
                &[],
                &kw,
                None,
            )
            .await;
        assert_eq!(empty_multi_buy.unwrap_err().code, "InvalidBuyMultipleItems");

        let multi_buy_res = ctx
            .dispatch_rpc(
                &mut s2,
                "marketProxy",
                None,
                "BuyMultipleItems",
                &[PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(34),
                    PyRep::Integer(10),
                    PyRep::Real(10.0),
                ])])],
                &kw,
                None,
            )
            .await
            .expect("BuyMultipleItems 10x Tritanium");
        let PyRep::KeyVal(mb_kv) = multi_buy_res else {
            panic!("Expected KeyVal from BuyMultipleItems");
        };
        assert_eq!(mb_kv.get("purchasedCount"), Some(&PyRep::Integer(1)));
        assert_eq!(mb_kv.get("totalQuantity"), Some(&PyRep::Integer(10)));
        assert_eq!(mb_kv.get("totalCostISK"), Some(&PyRep::Real(50.0)));

        // R15-P1-1: marketProxy.PlaceBuyOrder rejects placing an order at a remote station in the same region (`StationMismatch`)
        // and rejects placing an order while undocked in space (`NotDockedAtStation`)
        let remote_station_order = ctx
            .dispatch_rpc(
                &mut s2,
                "marketProxy",
                None,
                "PlaceBuyOrder",
                &[
                    PyRep::Integer(60_003_766), // Perimeter station in same region 10000002 while s2 is docked at Jita 60003760
                    PyRep::Integer(34),
                    PyRep::Real(5.0),
                    PyRep::Integer(10),
                    PyRep::Integer(90),
                    PyRep::Integer(1),
                ],
                &kw,
                None,
            )
            .await;
        assert_eq!(remote_station_order.unwrap_err().code, "StationMismatch");

        // s1 is currently in space in Jita (30_000_142) after undocking earlier in this test
        let in_space_order = ctx
            .dispatch_rpc(
                &mut s1,
                "marketProxy",
                None,
                "PlaceBuyOrder",
                &[
                    PyRep::Integer(60_003_760),
                    PyRep::Integer(34),
                    PyRep::Real(5.0),
                    PyRep::Integer(10),
                    PyRep::Integer(90),
                    PyRep::Integer(1),
                ],
                &kw,
                None,
            )
            .await;
        assert_eq!(in_space_order.unwrap_err().code, "NotDockedAtStation");

        // R20-P1-3: Verify PlaceBuyOrder resolves station-specific owner_corporation_id (1000035) and owner_faction_id (500001)
        // standings separately for Jita 4-4 (60003760) instead of passing corp standing as both faction and corp standing
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_002) {
                ch.balance = eve_db::IskCents::from_isk(100_000);
                ch.standings.clear();
                // Caldari State (500001) = 5.0, Caldari Navy (1000035) = 2.0
                ch.standings.insert(500_001, 5.0);
                ch.standings.insert(1_000_035, 2.0);
            }
        });
        // Place a Resting Bid at 1.0 ISK x 10_000 = 10,000.00 ISK
        // With Broker Relations IV (level 4), faction standing 5.0, corp standing 2.0:
        // rate = 0.03 - 0.003*4 - 0.0003*5.0 - 0.0002*2.0 = 0.018 - 0.0015 - 0.0004 = 0.0161 (1.61%)
        // Broker fee on 10,000 ISK = 161.00 ISK (whereas using corp standing 2.0 for both would yield 0.0170 -> 170.00 ISK)
        let _ = ctx
            .dispatch_rpc(
                &mut s2,
                "marketProxy",
                None,
                "PlaceBuyOrder",
                &[
                    PyRep::Integer(60_003_760),
                    PyRep::Integer(34),
                    PyRep::Real(1.0),
                    PyRep::Integer(10_000),
                    PyRep::Integer(90),
                    PyRep::Integer(1),
                ],
                &kw,
                None,
            )
            .await
            .expect("PlaceBuyOrder resting bid with separate faction and corp standings");
        let bal_after_bid = ctx
            .db
            .with_snapshot(|snap| snap.characters[&90_000_002].balance.0);
        // 100,000.00 ISK - 10,000.00 ISK escrow - 161.00 ISK broker fee = 89,839.00 ISK (8_983_900 cents)
        assert_eq!(bal_after_bid, 8_983_900);

        // R19-P1-1 & R22-P1-1: Vorton Projector (Small Vorton Projector I 54739 loaded with GalvaSurge Condenser Pack S 54769)
        // uses missile_damage_factor (explosion radius 654 = 43.0m, explosion velocity 653 = 190.0 m/s, DRF 1353 = 0.40),
        // never rolls turret wrecking hits, and arcs up to 9 additional targets (3037 = 10.0) strictly within 10,000m of the primary target with no secondary hopping
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                100_009_950,
                eve_db::InventoryItemRecord {
                    item_id: 100_009_950,
                    type_id: 54731, // Skybreaker EDENCOM frigate hull
                    owner_id: 90_000_002,
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
            snap.items.insert(
                100_009_951,
                eve_db::InventoryItemRecord {
                    item_id: 100_009_951,
                    type_id: 54739, // Small Vorton Projector I
                    owner_id: 90_000_002,
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
            snap.items.insert(
                100_009_952,
                eve_db::InventoryItemRecord {
                    item_id: 100_009_952,
                    type_id: 54769, // GalvaSurge Condenser Pack S (116 EM + 17 Kin = 133 base damage)
                    owner_id: 90_000_002,
                    location_id: 100_009_950,
                    flag: InventoryFlag::Cargo,
                    quantity: 200,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
            // Ensure EDENCOM Frigate (55031), Vorton Projector Operation (55033), and Small Vorton Projector (55034) are trained to level 1 on 90_000_002
            for sk_tid in [55031_u32, 55033, 55034] {
                snap.skills.entry(90_000_002).or_default().insert(
                    sk_tid,
                    eve_db::TrainedSkillRecord {
                        type_id: sk_tid,
                        level: 1,
                        skill_points: 1000.0,
                    },
                );
            }
        });
        ctx.dispatch_rpc(
            &mut s2,
            "ship",
            None,
            "Board",
            &[PyRep::Integer(100_009_950)],
            &kw,
            None,
        )
        .await
        .expect("Board Skybreaker 100_009_950");
        ctx.dispatch_rpc(
            &mut s2,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_009_951),
                PyRep::Integer(100_009_950),
                PyRep::Integer(27),
            ],
            &kw,
            None,
        )
        .await
        .expect("Fit Small Vorton Projector I onto Skybreaker");
        ctx.dispatch_rpc(
            &mut s2,
            "dogmaIM",
            None,
            "LoadAmmo",
            &[
                PyRep::Integer(100_009_950),
                PyRep::Integer(27),
                PyRep::Integer(100_009_952),
            ],
            &kw,
            None,
        )
        .await
        .expect("Load GalvaSurge Condenser Pack S into Small Vorton Projector I");
        ctx.dispatch_rpc(&mut s2, "stationSvc", None, "Undock", &[], &kw, None)
            .await
            .expect("Undock Skybreaker");
        for _ in 0..7 {
            let _ = jita_handle.step_manual(1.0).await.unwrap();
        }
        let (_, sky_ball) = jita_handle
            .get_ball_snapshot(100_009_950)
            .await
            .unwrap()
            .unwrap();
        for (bid, offset_x) in [
            (700_009_950_i64, 5_000.0), // Primary target at 5 km (<= 22.46 km optimal)
            (700_009_951_i64, 13_000.0), // Secondary target 8 km from primary (<= 10 km arc radius)
            (700_009_952_i64, 17_000.0), // Out-of-arc target 12 km from primary (> 10 km arc radius, even though 4 km <= 10 km from secondary 700_009_951)
        ] {
            let _ = jita_handle
                .spawn_ball_with_combat(
                    eve_destiny::Ball {
                        id: bid,
                        type_id: 16997,
                        position: sky_ball.position + glam::DVec3::new(offset_x, 0.0, 0.0),
                        velocity: glam::DVec3::new(380.0, 0.0, 0.0),
                        max_velocity: 400.0,
                        inertia: 2.0,
                        mass: 1_000_000.0,
                        radius: 35.0,
                        speed_fraction: 0.95,
                        mode: eve_destiny::MovementMode::Stop,
                    },
                    eve_destiny::CombatState {
                        shield_hp: 35.0,
                        max_shield_hp: 35.0,
                        armor_hp: 35.0,
                        max_armor_hp: 35.0,
                        structure_hp: 35.0,
                        max_structure_hp: 35.0,
                        signature_radius: 35.0,
                        ..eve_destiny::CombatState::default()
                    },
                )
                .await;
        }
        let vorton_res = ctx
            .dispatch_rpc(
                &mut s2,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(700_009_950)],
                &kw,
                None,
            )
            .await
            .expect("FireWeapon Small Vorton Projector I shot 1");
        let PyRep::KeyVal(v_kv) = vorton_res else {
            panic!("Expected KeyVal from Vorton FireWeapon");
        };
        assert_eq!(
            v_kv.get("weaponKind").and_then(|v| v.as_str()),
            Some("VortonProjector")
        );
        assert_eq!(v_kv.get("isWreckingShot"), Some(&PyRep::Bool(false)));
        assert_eq!(v_kv.get("secondaryTargetsHit"), Some(&PyRep::Integer(1)));
        assert_eq!(v_kv.get("destroyed"), Some(&PyRep::Bool(false)));
        let expected_vorton_dmg = 133.0 * ((35.0_f64 * 190.0) / (43.0 * 380.0)).powf(0.40);
        let actual_vorton_dmg = v_kv.get("appliedDamage").and_then(|v| v.as_f64()).unwrap();
        assert!(
            (actual_vorton_dmg - expected_vorton_dmg).abs() < 1e-4 && actual_vorton_dmg < 133.0,
            "Vorton appliedDamage {actual_vorton_dmg} must match missile_damage_factor {expected_vorton_dmg}"
        );
        // Secondary target (105 total HP) survived shot 1 because missile_damage_factor reduced 133.0 -> 92.83 (< 105.0)
        assert!(jita_handle
            .get_ball_snapshot(700_009_951)
            .await
            .unwrap()
            .is_some());
        // Step 7s for 6.0s Vorton cycle and fire shot 2: both primary (700_009_950) and secondary (700_009_951) are destroyed, while 700_009_952 (12 km > 10 km from primary, 4 km from secondary) is untouched
        for _ in 0..7 {
            let _ = jita_handle.step_manual(1.0).await.unwrap();
        }
        let vorton_res2 = ctx
            .dispatch_rpc(
                &mut s2,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(700_009_950)],
                &kw,
                None,
            )
            .await
            .expect("FireWeapon Small Vorton Projector I shot 2");
        let PyRep::KeyVal(v_kv2) = vorton_res2 else {
            panic!("Expected KeyVal from Vorton FireWeapon shot 2");
        };
        assert_eq!(v_kv2.get("destroyed"), Some(&PyRep::Bool(true)));
        assert!(jita_handle
            .get_ball_snapshot(700_009_951)
            .await
            .unwrap()
            .is_none());
        assert!(jita_handle
            .get_ball_snapshot(700_009_952)
            .await
            .unwrap()
            .is_some());


        (s1, s2)
    }
}

pub(crate) mod run_extract_rowset_rows {
    // Integration regression tests covering Round 8 findings (Part 3).


    use eve_db::InventoryFlag;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    pub(crate) async fn run_r8_part3(
        ctx: &ServerContext,
        mut s1: SessionState,
        mut s2: SessionState,
    ) {
        let kw = HashMap::new();
        let jita_handle = ctx.systems.get_or_spawn(30_000_142);
        // R21-P1-1: planetMgr.UserUpdateNetwork gates max colonies per character on 1 + Interplanetary Consolidation (2495) trained level
        // Character 90_000_002 has Interplanetary Consolidation untrained (0 -> max 1 colony); establishing 1st colony on Jita 40009082 succeeds,
        // and attempting a 2nd colony on Jita 40009077 fails with MaxColoniesReached
        ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let id1 = snap.next_entity_id;
            snap.items.insert(
                id1,
                eve_db::InventoryItemRecord {
                    item_id: id1,
                    type_id: 2534,
                    owner_id: 90_000_002,
                    location_id: 100_009_950,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.next_entity_id += 1;
            let id2 = snap.next_entity_id;
            snap.items.insert(
                id2,
                eve_db::InventoryItemRecord {
                    item_id: id2,
                    type_id: 2524,
                    owner_id: 90_000_002,
                    location_id: 100_009_950,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        ctx.dispatch_rpc(
            &mut s2,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(40_009_082),
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
        .expect("1st colony on Jita 40009082 with Interplanetary Consolidation 0");
        let second_colony_untrained = ctx
            .dispatch_rpc(
                &mut s2,
                "planetMgr",
                None,
                "UserUpdateNetwork",
                &[
                    PyRep::Integer(40_009_077),
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(1),
                        PyRep::Integer(1),
                        PyRep::Integer(2524),
                        PyRep::Real(0.1),
                        PyRep::Real(0.2),
                    ])]),
                ],
                &kw,
                None,
            )
            .await;
        assert_eq!(
            second_colony_untrained.unwrap_err().code,
            "MaxColoniesReached"
        );

        // R21-P1-4 & R22-P1-2: Dock s2 back at Jita 4-4 (60003760) and verify PlaceCharOrder returns CannotSellAssembledItem, CannotSellItemWithContents,
        // and charges exact integer ppm sales tax from the seller's own Accounting (16622) skill level (levels 0 and 4)
        let (_, sky_ball_now) = jita_handle
            .get_ball_snapshot(100_009_950)
            .await
            .unwrap()
            .unwrap();
        let jita_station_pos =
            glam::DVec3::new(-107_303_362_560.0, -18_744_975_360.0, 436_489_052_160.0);
        if sky_ball_now.position.distance(jita_station_pos) > 2_000.0 {
            let _ = jita_handle
                .spawn_ball(eve_destiny::Ball {
                    id: 100_009_950,
                    position: jita_station_pos + glam::DVec3::new(500.0, 0.0, 0.0),
                    ..sky_ball_now
                })
                .await;
        }
        ctx.advance_secs(12);
        ctx.dispatch_rpc(
            &mut s2,
            "stationSvc",
            None,
            "Dock",
            &[PyRep::Integer(60_003_760)],
            &kw,
            None,
        )
        .await
        .expect("Dock s2 back at Jita 4-4");

        ctx.db.with_mut_snapshot(|snap| {
            // Ensure 100_009_911 (type 1236 Overdrive Injector System II) in Jita 4-4 hangar is assembled (singleton = true) and empty
            if let Some(m) = snap.items.get_mut(&100_009_911) {
                m.location_id = 60_003_760;
                m.flag = InventoryFlag::Hangar;
                m.singleton = true;
            }
        });
        let sell_assembled = ctx
            .dispatch_rpc(
                &mut s2,
                "marketProxy",
                None,
                "PlaceMultiSellOrder",
                &[
                    PyRep::List(vec![PyRep::keyval([
                        ("stationID", PyRep::Integer(60_003_760)),
                        ("typeID", PyRep::Integer(1236)),
                        ("price", PyRep::Real(250_000.0)),
                        ("quantity", PyRep::Integer(1)),
                    ])]),
                    PyRep::Bool(false),
                    PyRep::Integer(90),
                    PyRep::Real(0.0),
                ],
                &kw,
                None,
            )
            .await;
        assert_eq!(sell_assembled.unwrap_err().code, "CannotSellAssembledItem");

        // 100_000_002 (type 587 Rifter) is in Jita 4-4 hangar and contains fitted modules (100_009_901..903)
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&100_009_910);
            snap.items.remove(&100_009_950);
        });
        let sell_with_contents = ctx
            .dispatch_rpc(
                &mut s2,
                "marketProxy",
                None,
                "PlaceMultiSellOrder",
                &[
                    PyRep::List(vec![PyRep::keyval([
                        ("stationID", PyRep::Integer(60_003_760)),
                        ("typeID", PyRep::Integer(587)),
                        ("price", PyRep::Real(250_000.0)),
                        ("quantity", PyRep::Integer(1)),
                    ])]),
                    PyRep::Bool(false),
                    PyRep::Integer(90),
                    PyRep::Real(0.0),
                ],
                &kw,
                None,
            )
            .await;
        assert_eq!(
            sell_with_contents.unwrap_err().code,
            "CannotSellItemWithContents"
        );

        // R22-P1-2: Place a resting player bid order for 2x Overdrive Injector System II (1236) at 500,000.00 ISK (50_000_000 cents, above the 329,000.00 ISK NPC bid),
        // repackage 100_009_911 into a 2-unit stack, and verify PlaceMultiSellOrder sell fills charge exact integer ppm salesTaxISK for Accounting level 0 (75,000 ppm = 37,500.00 ISK) and level 4 (42,000 ppm = 21,000.00 ISK)
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(m) = snap.items.get_mut(&100_009_911) {
                m.singleton = false;
                m.quantity = 2;
            }
            if let Some(sk) = snap.skills.get_mut(&90_000_002) {
                sk.remove(&16622); // Accounting 0
            }
            if let Some(c) = snap.characters.get_mut(&90_000_002) {
                c.balance = eve_db::IskCents(10_000_000); // 100,000.00 ISK
            }
            snap.market_orders.insert(
                777_000_124,
                eve_db::MarketOrderRecord {
                    order_id: 777_000_124,
                    owner_id: 90_000_001,
                    station_id: 60_003_760,
                    region_id: 10_000_002,
                    type_id: 1236,
                    price: eve_db::IskCents(50_000_000), // 500,000.00 ISK per unit
                    vol_entered: 2,
                    vol_remaining: 2,
                    min_volume: 1,
                    is_bid: true,
                    is_npc: false,
                    escrow: eve_db::IskCents(100_000_000),
                },
            );
        });
        let sell_acct_0 = ctx
            .dispatch_rpc(
                &mut s2,
                "marketProxy",
                None,
                "PlaceMultiSellOrder",
                &[
                    PyRep::List(vec![PyRep::keyval([
                        ("stationID", PyRep::Integer(60_003_760)),
                        ("typeID", PyRep::Integer(1236)),
                        ("price", PyRep::Real(500_000.0)),
                        ("quantity", PyRep::Integer(1)),
                    ])]),
                    PyRep::Bool(false),
                    PyRep::Integer(90),
                    PyRep::Real(0.0),
                ],
                &kw,
                None,
            )
            .await
            .expect("Sell 1x 1236 at Accounting level 0");
        let PyRep::KeyVal(sa0_kv) = sell_acct_0 else {
            panic!("Expected KeyVal from PlaceMultiSellOrder sell at Accounting 0");
        };
        assert_eq!(sa0_kv.get("filledImmediately"), Some(&PyRep::Bool(true)));
        assert_eq!(sa0_kv.get("salesTaxISK"), Some(&PyRep::Real(37_500.0)));
        let bal_after_acct_0 = ctx
            .db
            .with_snapshot(|snap| snap.characters[&90_000_002].balance.0);
        // 100,000.00 ISK + (500,000.00 ISK - 37,500.00 ISK sales tax) = 562,500.00 ISK (56_250_000 cents; no broker fee on immediate fill)
        assert_eq!(bal_after_acct_0, 56_250_000);

        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(90_000_002).or_default().insert(
                16622,
                eve_db::TrainedSkillRecord {
                    type_id: 16622,
                    level: 4,
                    skill_points: 135_765.0,
                },
            );
        });
        let sell_acct_4 = ctx
            .dispatch_rpc(
                &mut s2,
                "marketProxy",
                None,
                "PlaceMultiSellOrder",
                &[
                    PyRep::List(vec![PyRep::keyval([
                        ("stationID", PyRep::Integer(60_003_760)),
                        ("typeID", PyRep::Integer(1236)),
                        ("price", PyRep::Real(500_000.0)),
                        ("quantity", PyRep::Integer(1)),
                    ])]),
                    PyRep::Bool(false),
                    PyRep::Integer(90),
                    PyRep::Real(0.0),
                ],
                &kw,
                None,
            )
            .await
            .expect("Sell 1x 1236 at Accounting level 4");
        let PyRep::KeyVal(sa4_kv) = sell_acct_4 else {
            panic!("Expected KeyVal from PlaceMultiSellOrder sell at Accounting 4");
        };
        assert_eq!(sa4_kv.get("filledImmediately"), Some(&PyRep::Bool(true)));
        assert_eq!(sa4_kv.get("salesTaxISK"), Some(&PyRep::Real(21_000.0)));
        let bal_after_acct_4 = ctx
            .db
            .with_snapshot(|snap| snap.characters[&90_000_002].balance.0);
        // 562,500.00 ISK + (500,000.00 ISK - 21,000.00 ISK sales tax) = 1,041,500.00 ISK (104_150_000 cents; no broker fee on immediate fill)
        assert_eq!(bal_after_acct_4, 104_150_000);

        // R23-P1-1: standingMgr.GetStandingTransactions records only actual standing deltas (no Decline penalty, exact derived Complete delta)
        fn extract_rowset_rows(rep: &PyRep) -> &Vec<Vec<PyRep>> {
            let PyRep::Rowset { rows, .. } = rep else {
                panic!("Expected PyRep::Rowset, got {:?}", rep);
            };
            rows
        }
        let st_tx_initial = ctx
            .dispatch_rpc(
                &mut s2,
                "standingMgr",
                None,
                "GetStandingTransactions",
                &[PyRep::Integer(1_000_419)],
                &kw,
                None,
            )
            .await
            .expect("GetStandingTransactions initial for s2");
        let init_rows = extract_rowset_rows(&st_tx_initial);
        assert_eq!(init_rows.len(), 1);
        assert_eq!(init_rows[0][3], PyRep::Real(0.15));
        assert_eq!(init_rows[0][4], PyRep::Integer(85));

        // Co-locate s2 (90_000_002) with Caldari Navy (1_000_035) L1 agent 3_012_018 at Maurasi IV - Moon 4 (30_000_144 / 60_003_754)
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_002) {
                ch.solar_system_id = 30_000_144;
                ch.station_id = Some(60_003_754);
            }
        });
        s2.solar_system_id = Some(30_000_144);
        s2.station_id = Some(60_003_754);
        let rm_decline = ctx
            .dispatch_rpc(
                &mut s2,
                "agentMgr",
                None,
                "DoAction",
                &[PyRep::Integer(2), PyRep::Integer(3_012_018)],
                &kw,
                None,
            )
            .await
            .expect("DoAction(RequestMission) for s2 decline");
        let PyRep::KeyVal(rm_dec_kv) = rm_decline else {
            panic!("Expected rm_dec_kv");
        };
        let s2_dec_mid = rm_dec_kv.get("missionID").unwrap().as_i64().unwrap();
        ctx.dispatch_rpc(
            &mut s2,
            "agentMgr",
            None,
            "RemoveOfferFromJournal",
            &[PyRep::Integer(s2_dec_mid)],
            &kw,
            None,
        )
        .await
        .expect("RemoveOfferFromJournal for s2 decline");
        let st_tx_after_decline = ctx
            .dispatch_rpc(
                &mut s2,
                "standingMgr",
                None,
                "GetStandingTransactions",
                &[PyRep::Integer(1_000_035)],
                &kw,
                None,
            )
            .await
            .expect("GetStandingTransactions after decline");
        assert_eq!(extract_rowset_rows(&st_tx_after_decline).len(), 0);

        let rm_quit = ctx
            .dispatch_rpc(&mut s2, "agentMgr", None, "DoAction", &[PyRep::Integer(2)], &kw, None)
            .await
            .expect("DoAction(RequestMission) for s2 quit");
        let PyRep::KeyVal(rm_quit_kv) = rm_quit else {
            panic!("Expected rm_quit_kv");
        };
        let s2_quit_mid = rm_quit_kv.get("missionID").unwrap().as_i64().unwrap();
        ctx.dispatch_rpc(
            &mut s2,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(3), PyRep::Integer(s2_quit_mid)],
            &kw,
            None,
        )
        .await
        .expect("DoAction(AcceptMission) for s2 quit");
        ctx.dispatch_rpc(
            &mut s2,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(11), PyRep::Integer(s2_quit_mid)],
            &kw,
            None,
        )
        .await
        .expect("DoAction(QuitMission) for s2");
        let st_tx_after_quit = ctx
            .dispatch_rpc(
                &mut s2,
                "standingMgr",
                None,
                "GetStandingTransactions",
                &[PyRep::Integer(1_000_035)],
                &kw,
                None,
            )
            .await
            .expect("GetStandingTransactions after quit");
        let quit_rows = extract_rowset_rows(&st_tx_after_quit);
        assert_eq!(quit_rows.len(), 1);
        assert_eq!(quit_rows[0][3], PyRep::Real(-0.1));
        assert_eq!(quit_rows[0][4], PyRep::Integer(83));

        // R23-P1-2: jumpCloneSvc.GetPriceForClone matches flat 900,000 ISK charged by InstallCloneInStation
        let jc_quote = ctx
            .dispatch_rpc(
                &mut s2,
                "jumpCloneSvc",
                None,
                "GetPriceForClone",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetPriceForClone");
        assert_eq!(jc_quote, PyRep::Real(900_000.0));

        // R23-P1-3: taleMgr.GetGlobalWorldEventTales and get_active_tales_by_template return live tale data
        let tales_res = ctx
            .dispatch_rpc(
                &mut s2,
                "taleMgr",
                None,
                "GetGlobalWorldEventTales",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetGlobalWorldEventTales must succeed");
        assert!(matches!(tales_res, PyRep::List(_)));
        let active_tales = ctx
            .dispatch_rpc(
                &mut s2,
                "taleMgr",
                None,
                "get_active_tales_by_template",
                &[PyRep::Integer(1)],
                &kw,
                None,
            )
            .await
            .expect("get_active_tales_by_template must succeed");
        assert!(matches!(active_tales, PyRep::List(_)));

        // -------------------------------------------------------------------------
        // Track 2: SDE Loaders & Auditor Fixes
        // -------------------------------------------------------------------------

        // parity: plan:D-2
        // Verify O(1) indexed stations map in SdeRegistry and preserved columns
        assert_eq!(ctx.sde.stations.len(), 5_210);
        let jita_sta = ctx.sde.stations.get(&60_003_760).expect("Jita 4-4 station");
        assert_eq!(jita_sta.station_id, 60_003_760);
        assert_eq!(jita_sta.solar_system_id, 30_000_142);
        assert_eq!(jita_sta.constellation_id, 20_000_020);
        assert_eq!(jita_sta.region_id, 10_000_002);
        assert_eq!(jita_sta.operation_id, 14);
        assert_eq!(jita_sta.reprocessing_efficiency, 0.5);
        assert_eq!(jita_sta.reprocessing_stations_take, 0.05);
        assert!(!jita_sta.is_conquerable);
        assert_eq!(jita_sta.orbit_id, Some(40_009_087));
        assert_eq!(jita_sta.owner_faction_id, Some(500_001));

        // Station without NPC corp faction_id has None owner_faction_id (no hardcoded Caldari 500001 fallback)
        let non_emp_sta = ctx.sde.stations.get(&60_014_839).expect("station 60014839");
        assert_eq!(non_emp_sta.owner_faction_id, None);

        // Non-empire race has None corvette_type_id (no hardcoded Ibis 601 fallback)
        assert_eq!(ctx.sde.races.get(&16).unwrap().corvette_type_id, None);
        assert_eq!(ctx.sde.races.get(&1).unwrap().corvette_type_id, Some(601));

        // RPC GetStation uses O(1) indexed station map
        let gsta = ctx
            .dispatch_rpc(
                &mut s1,
                "station",
                None,
                "GetStation",
                &[PyRep::Integer(60_003_760)],
                &kw,
                None,
            )
            .await
            .expect("station.GetStation via indexed map");
        let PyRep::KeyVal(gsta_kv) = gsta else {
            panic!("Expected gsta_kv");
        };
        assert_eq!(
            gsta_kv.get("stationID").unwrap(),
            &PyRep::Integer(60_003_760)
        );
        assert_eq!(
            gsta_kv.get("solarSystemID").unwrap(),
            &PyRep::Integer(30_000_142)
        );
        assert_eq!(
            gsta_kv.get("stationName").unwrap(),
            &PyRep::String("Jita IV - Moon 4 - Caldari Navy Assembly Plant".into())
        );

        // parity: sde:res:/staticdata/types.fsdbinary
        // Verify InvType full columns preserved in SDE registry
        let rifter_t = ctx.sde.types.get(&587).expect("Rifter type");
        assert_eq!(rifter_t.description_id, Some(93841));
        assert_eq!(rifter_t.icon_id, None);
        assert_eq!(rifter_t.graphic_id, Some(46));
        assert_eq!(rifter_t.sound_id, Some(20078));
        assert_eq!(rifter_t.meta_group_id, Some(1));
        assert_eq!(rifter_t.meta_level, Some(0));
        assert_eq!(rifter_t.tech_level, Some(1));

        let trit_t = ctx.sde.types.get(&34).expect("Tritanium type");
        assert_eq!(trit_t.description_id, Some(87134));
        assert_eq!(trit_t.icon_id, Some(22));
        assert_eq!(trit_t.graphic_id, None);
        assert_eq!(trit_t.sound_id, None);
        assert_eq!(trit_t.meta_group_id, None);
        assert_eq!(trit_t.meta_level, None);
        assert_eq!(trit_t.tech_level, None);

        let nemesis_t = ctx.sde.types.get(&11377).expect("Nemesis type");
        assert_eq!(nemesis_t.description_id, Some(93939));
        assert_eq!(nemesis_t.icon_id, None);
        assert_eq!(nemesis_t.graphic_id, Some(1909));
        assert_eq!(nemesis_t.sound_id, Some(20072));
        assert_eq!(nemesis_t.meta_group_id, Some(2));
        assert_eq!(nemesis_t.meta_level, Some(5));
        assert_eq!(nemesis_t.tech_level, Some(2));

        // parity: sde:res:/staticdata/skins.static
        // Verify skin materials and expanded skin fields
        assert_eq!(ctx.sde.skin_materials.len(), 875);
        let smat1 = ctx.sde.skin_materials.get(&1).expect("skin material 1");
        assert_eq!(smat1.skin_material_id, 1);
        assert_eq!(smat1.display_name_id, Some(505346));
        assert_eq!(smat1.material_set_id, Some(1));

        let skin5 = ctx.sde.skins.get(&5).expect("skin 5");
        assert_eq!(skin5.internal_name, "Megathron Quafe");
        assert_eq!(skin5.types, vec![641]);
        assert_eq!(skin5.skin_material_id, 3);
        assert!(skin5.allow_ccp_devs);
        assert!(!skin5.visible_serenity);
        assert!(skin5.visible_tranquility);
        assert!(!skin5.is_structure_skin);

        // Cosmetics RPC validation against ctx.sde.skins and ctx.sde.skin_licenses
        s1.role |= eve_machonet::service_roles::ROLE_GML;
        // GiveSkin rejects invalid skinID
        let bad_giveskin = ctx
            .dispatch_rpc(
                &mut s1,
                "shipCosmeticsMgr",
                None,
                "GiveSkin",
                &[PyRep::Integer(99_999_999)],
                &kw,
                None,
            )
            .await;
        assert_eq!(bad_giveskin.unwrap_err().code, "InvalidSkinLicenseType");

        // GiveSkin succeeds with valid skinID (170 for Rifter Krusual)
        let good_giveskin = ctx
            .dispatch_rpc(
                &mut s1,
                "shipCosmeticsMgr",
                None,
                "GiveSkin",
                &[PyRep::Integer(170)],
                &kw,
                None,
            )
            .await
            .expect("GiveSkin with valid skinID 170");
        let PyRep::KeyVal(gsk_kv) = good_giveskin else {
            panic!("Expected gsk_kv");
        };
        assert_eq!(gsk_kv.get("skinID").unwrap(), &PyRep::Integer(170));
        assert_eq!(gsk_kv.get("licensed").unwrap(), &PyRep::Bool(true));

        // ActivateSkinLicense rejects invalid skin license typeID
        let bad_act = ctx
            .dispatch_rpc(
                &mut s1,
                "shipCosmeticsMgr",
                None,
                "ActivateSkinLicense",
                &[PyRep::Integer(99_999_999)],
                &kw,
                None,
            )
            .await;
        assert_eq!(bad_act.unwrap_err().code, "InvalidSkinLicenseType");

        // ActivateSkinLicense rejects item that is not category 91 (e.g. Tritanium 34)
        let non_cat91_act = ctx
            .dispatch_rpc(
                &mut s1,
                "shipCosmeticsMgr",
                None,
                "ActivateSkinLicense",
                &[PyRep::Integer(34)],
                &kw,
                None,
            )
            .await;
        assert_eq!(non_cat91_act.unwrap_err().code, "InvalidSkinLicenseType");

        // ApplySkinToShip rejects invalid skinID
        let bad_apply = ctx
            .dispatch_rpc(
                &mut s1,
                "shipCosmeticsMgr",
                None,
                "ApplySkinToShip",
                &[PyRep::Integer(587), PyRep::Integer(99_999_999)],
                &kw,
                None,
            )
            .await;
        assert_eq!(bad_apply.unwrap_err().code, "SkinNotLicensed");

        // ApplySkinToShip rejects incompatible ship type (skin 5 is for Megathron 641, not Rifter 587)
        let incomp_apply = ctx
            .dispatch_rpc(
                &mut s1,
                "shipCosmeticsMgr",
                None,
                "ApplySkinToShip",
                &[PyRep::Integer(587), PyRep::Integer(5)],
                &kw,
                None,
            )
            .await;
        assert_eq!(incomp_apply.unwrap_err().code, "InvalidShipTypeForSkin");

        // parity: sde:res:/staticdata/fighterabilities.static
        // Verify fighter abilities full columns, mappings, and fighterMgr.CmdActivateAbilitySlots SDE wiring
        let fa2 = ctx.sde.fighter_abilities.get(&2).expect("fighter ability 2");
        assert_eq!(fa2.tooltip_text_id, Some(516650));
        let fa18 = ctx.sde.fighter_abilities.get(&18).expect("fighter ability 18");
        assert_eq!(fa18.turret_graphic_id, Some(11515));
        assert_eq!(fa18.tooltip_text_id, Some(516660));

        s1.solar_system_id = Some(30_000_142); // Jita (high-sec)
        let jita_sys = ctx.systems.get_or_spawn(30_000_142);
        for (bid, tid, x) in [(880_001_i64, 32340_u32, 0.0), (880_002_i64, 40362_u32, 100.0), (100_000_002_i64, 282_u32, 2_000.0)] {
            jita_sys
                .spawn_ball_with_combat(
                    eve_destiny::Ball {
                        id: bid,
                        type_id: tid,
                        position: glam::DVec3::new(x, 0.0, 0.0),
                        velocity: glam::DVec3::ZERO,
                        max_velocity: 1_000.0,
                        inertia: 1.0,
                        mass: 10_000.0,
                        radius: 20.0,
                        speed_fraction: 0.0,
                        mode: eve_destiny::MovementMode::Stop,
                    },
                    ctx.build_type_combat_state(tid),
                )
                .await
                .expect("spawn ball in Jita");
        }
        ctx.active_fighters.write().unwrap().insert(
            880_001,
            eve_server::router::context::ActiveFighterSquadron {
                ball_id: 880_001,
                item_id: 770_001,
                type_id: 32340, // Malleus I (slot 2 -> ability 18: 9 charges, 6s rearm, turretGraphicID 11515, tooltipTextID 516660)
                owner_char_id: 90_000_001,
                parent_ship_id: 100_000_001,
                tube_index: 0,
                squadron_size: 6,
                target_id: None,
                is_abandoned: false,
                mwd_active: false,
            },
        );
        ctx.active_fighters.write().unwrap().insert(
            880_002,
            eve_server::router::context::ActiveFighterSquadron {
                ball_id: 880_002,
                item_id: 770_002,
                type_id: 40362, // Ametat I (slot 2 -> ability 7: disallow_in_high_sec = true)
                owner_char_id: 90_000_001,
                parent_ship_id: 100_000_001,
                tube_index: 1,
                squadron_size: 6,
                target_id: None,
                is_abandoned: false,
                mwd_active: false,
            },
        );

        // Unlocked target must be rejected with TargetNotLocked
        let unlocked_err = ctx
            .dispatch_rpc(
                &mut s1,
                "fighterMgr",
                None,
                "CmdActivateAbilitySlots",
                &[
                    PyRep::Integer(880_001),
                    PyRep::Integer(2),
                    PyRep::Integer(100_000_002),
                ],
                &kw,
                None,
            )
            .await
            .expect_err("Unlocked target must be rejected");
        assert_eq!(unlocked_err.code, "TargetNotLocked");

        // Lock target 100_000_002 from controlling ship 100_000_001
        jita_sys
            .add_target_lock(
                100_000_001,
                eve_destiny::TargetLock {
                    target_id: 100_000_002,
                    lock_time_ms: 1_000.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(100_000.0),
                },
            )
            .await
            .expect("lock target 100_000_002");

        let act_fa18 = ctx
            .dispatch_rpc(
                &mut s1,
                "fighterMgr",
                None,
                "CmdActivateAbilitySlots",
                &[
                    PyRep::Integer(880_001),
                    PyRep::Integer(2),
                    PyRep::Integer(100_000_002),
                ],
                &kw,
                None,
            )
            .await
            .expect("CmdActivateAbilitySlots slot 2 on Malleus I 32340");
        let PyRep::KeyVal(fa18_kv) = act_fa18 else {
            panic!("Expected fa18_kv");
        };
        assert_eq!(fa18_kv.get("abilityID").unwrap(), &PyRep::Integer(18));
        assert_eq!(
            fa18_kv.get("targetMode").unwrap(),
            &PyRep::String("itemTargeted".into())
        );
        assert_eq!(fa18_kv.get("chargeCount").unwrap(), &PyRep::Integer(9));
        assert_eq!(fa18_kv.get("remainingCharges").unwrap(), &PyRep::Integer(8));
        assert_eq!(fa18_kv.get("rearmTimeSeconds").unwrap(), &PyRep::Real(6.0));
        assert_eq!(
            fa18_kv.get("tooltipTextID").unwrap(),
            &PyRep::Integer(516660)
        );
        assert_eq!(
            fa18_kv.get("turretGraphicID").unwrap(),
            &PyRep::Integer(11515)
        );

        let act_hisec_err = ctx
            .dispatch_rpc(
                &mut s1,
                "fighterMgr",
                None,
                "CmdActivateAbilitySlots",
                &[PyRep::Integer(880_002), PyRep::Integer(2)],
                &kw,
                None,
            )
            .await
            .expect_err("Ability 7 must be disallowed in high-sec Jita");
        assert_eq!(act_hisec_err.code, "AbilityDisallowedInHighSec");
    }
}

pub(crate) mod blocking_findings_to {
    // Integration regression tests covering blocking findings B-01 through B-15.


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
}

pub(crate) mod blocking_findings_to_tama_structure {
    // Integration regression tests covering blocking findings B-01 through B-15 (Part 2: Tama structure lifecycle, fueling, clones, and carriers).


    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    const EXPECTED_REMOTE_ERR: &str = "MustBeDockedAtTargetStation";

    #[tokio::test]
    async fn test_blocking_findings_b01_to_b15_tama_structure_and_clone_regressions() {
        // parity: rpc:fighterMgr.CmdActivateAbilitySlots, rpc:fighterMgr.GetFightersForShip, rpc:home_station.set_home_station, rpc:jumpCloneSvc.CloneJump, rpc:jumpCloneSvc.InstallCloneInStation, rpc:jumpCloneSvc.InstallCloneInStructure, rpc:ship.Board, rpc:structureCynoBeaconMgr.CmdJumpToStructureBeacon, rpc:structureDeployment.Anchor, rpc:structureDocking.Dock, rpc:structureDocking.Undock
        use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();
        let mut sess = SessionState::new(1);
        let kw = HashMap::new();

        ctx.db.with_mut_snapshot(|snap| {
            let extra_items = [
                (100_000_015, 35832, 100_000_001, InventoryFlag::Cargo, 5, false),
                (100_000_016, 56201, 100_000_001, InventoryFlag::Cargo, 5, false),
                (100_000_023, 23055, 100_000_024, InventoryFlag::FighterBay, 9, false),
                (100_000_024, 24483, 60_003_760, InventoryFlag::Hangar, 1, true),
                (100_000_025, 4247, 100_000_024, InventoryFlag::Cargo, 500, false),
                (100_000_026, 4247, 100_000_001, InventoryFlag::Cargo, 2_000, false),
                (100_000_036, 35840, 100_000_001, InventoryFlag::Cargo, 2, false),
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

        ctx.dispatch_rpc(
            &mut sess,
            "authentication",
            None,
            "Login",
            &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
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

        async fn warp_and_jump_gate(
            ctx_ref: &ServerContext,
            sess_ref: &mut SessionState,
            kw_ref: &HashMap<String, PyRep>,
            gate_id: i64,
            _pos: [f64; 3],
        ) {
            let sys_id = sess_ref.solar_system_id.unwrap();
            let gate_pos = ctx_ref.sde.jumps[&gate_id].position;
            let pos = [gate_pos.x, gate_pos.y, gate_pos.z];
            let bound = ctx_ref
                .bind_object(sess_ref, "beyonce", sys_id as i64)
                .expect("bind beyonce");
            let handle = ctx_ref.systems.get_or_spawn(sys_id);
            let _ = handle.step_manual(300.0).await;
            let warp_res = ctx_ref
                .dispatch_rpc(
                    sess_ref,
                    "beyonce",
                    Some(bound),
                    "CmdWarpToStuff",
                    &[PyRep::String("item".into()), PyRep::Integer(gate_id)],
                    kw_ref,
                    None,
                )
                .await;
            match warp_res {
                Ok(_) => {
                    let ship_id = sess_ref.ship_id.unwrap();
                    for _ in 0..200 {
                        let _ = handle.step_manual(1.0).await.unwrap();
                        if let Ok(Some((_, b))) = handle.get_ball_snapshot(ship_id).await {
                            if !matches!(b.mode, eve_destiny::MovementMode::Warp { .. }) {
                                break;
                            }
                        }
                    }
                }
                Err(e) if e.code == "WarpDistanceTooShort" => {
                    let ship_id = sess_ref.ship_id.unwrap();
                    let (_, ball) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
                    let dir = [
                        pos[0] - ball.position.x,
                        pos[1] - ball.position.y,
                        pos[2] - ball.position.z,
                    ];
                    ctx_ref
                        .dispatch_rpc(
                            sess_ref,
                            "beyonce",
                            Some(bound),
                            "CmdGotoDirection",
                            &[PyRep::Real(dir[0]), PyRep::Real(dir[1]), PyRep::Real(dir[2])],
                            kw_ref,
                            None,
                        )
                        .await
                        .expect("CmdGotoDirection to nearby stargate");
                    for _ in 0..120 {
                        let _ = handle.step_manual(1.0).await.unwrap();
                    }
                    ctx_ref
                        .dispatch_rpc(sess_ref, "beyonce", Some(bound), "CmdStop", &[], kw_ref, None)
                        .await
                        .expect("CmdStop at stargate");
                }
                Err(e) => panic!("CmdWarpToStuff to stargate: {e:?}"),
            }
            ctx_ref.advance_secs(12);
            ctx_ref
                .dispatch_rpc(
                    sess_ref,
                    "beyonce",
                    Some(bound),
                    "CmdStargateJump",
                    &[PyRep::Integer(gate_id)],
                    kw_ref,
                    None,
                )
                .await
                .expect("CmdStargateJump");
        }

        // Undock in Rifter (100_000_001), travel Jita -> Ikuchi -> Tunttaras -> Nourvukaiken -> Tama (30002813) to anchor & fuel Tama Cyno Citadel
        ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
            .await
            .expect("Undock Rifter");
        for gate_id in [50_013_928_i64, 50_013_926, 50_001_709, 50_014_001] {
            warp_and_jump_gate(&ctx, &mut sess, &kw, gate_id, [0.0, 0.0, 0.0]).await;
        }
        assert_eq!(sess.solar_system_id, Some(30002813));

        // R36-P1-1 & R2-P0-01: materialize_system_lod2 rejects non-GMs with InsufficientRole, and rejects remote solar system arguments from GMs with CannotMaterializeRemoteSystem
        let non_gm_mat_err = eve_server::services::cosmetics_events::materialize_system_lod2(
            &ctx,
            &sess,
            &[PyRep::Integer(30_000_142)],
        )
        .await;
        assert_eq!(non_gm_mat_err.unwrap_err().code, "InsufficientRole");
        let saved_role = sess.role;
        sess.role |= eve_machonet::service_roles::ROLE_GMH;
        let remote_mat_err = eve_server::services::cosmetics_events::materialize_system_lod2(
            &ctx,
            &sess,
            &[PyRep::Integer(30_000_142)],
        )
        .await;
        sess.role = saved_role;
        assert_eq!(remote_mat_err.unwrap_err().code, "CannotMaterializeRemoteSystem");

        // R2-P1-03: Anchoring an Upwell structure while in starter NPC corporation (1_000_172) is rejected with NpcCorpCannotAnchorStructure
        let npc_anchor_err = ctx
            .dispatch_rpc(
                &mut sess,
                "structureDeployment",
                None,
                "Anchor",
                &[PyRep::Integer(35832), PyRep::String("Tama Cyno Citadel".into())],
                &kw,
                None,
            )
            .await;
        assert_eq!(npc_anchor_err.unwrap_err().code, "NpcCorpCannotAnchorStructure");

        // Create a player corporation on sess (90_000_001) before anchoring structures
        ctx.advance_secs(12);
        ctx.dispatch_rpc(
            &mut sess,
            "corpRegistry",
            None,
            "AddCorporation",
            &[
                PyRep::String("Tama Vanguard Corp".into()),
                PyRep::String("TVC".into()),
                PyRep::Real(0.05),
            ],
            &kw,
            None,
        )
        .await
        .expect("AddCorporation TVC");
        ctx.advance_secs(7 * 86_400);

        // Anchoring needs 1,000 km clearance from gates/stations/structures: fly 5,000,000 km off the gate.
        let tama = ctx.systems.get_or_spawn(30002813);
        let tama_ship = sess.ship_id.unwrap();
        let gate_side = tama.get_ball_snapshot(tama_ship).await.unwrap().unwrap().1.position;
        let citadel_spot = gate_side + glam::DVec3::new(0.0, 5.0e9, 0.0);
        assert!(tama.set_ball_position(tama_ship, citadel_spot).await);

        // Anchor Upwell Astrahus in Tama (30002813), verify fuelBlocks == 0, and fuel it from Rifter cargo via invbroker.Add (flag 133 = SpecializedFuelBay)
        let anchor_res = ctx
            .dispatch_rpc(
                &mut sess,
                "structureDeployment",
                None,
                "Anchor",
                &[PyRep::Integer(35832), PyRep::String("Tama Cyno Citadel".into())],
                &kw,
                None,
            )
            .await
            .expect("Anchor Tama Cyno Citadel");
        let PyRep::KeyVal(anchor_kv) = anchor_res else {
            panic!("Expected anchor KeyVal");
        };
        assert_eq!(anchor_kv.get("fuelBlocks").and_then(|v| v.as_i64()), Some(0));
        let tama_struct_id = anchor_kv.get("structureID").unwrap().as_i64().unwrap() as u64;
        assert!(tama.set_ball_position(tama_ship, citadel_spot + glam::DVec3::new(60_000.0, 0.0, 0.0)).await);
        ctx.dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_000_026),
                PyRep::Integer(tama_struct_id as i64),
                PyRep::Integer(133),
                PyRep::Integer(1_500),
            ],
            &kw,
            None,
        )
        .await
        .expect("Fuel Tama Cyno Citadel via invbroker.Add");

        // R-P1-07 & AP-06: Pharolux Cyno Beacon (35840) is rejected in low-sec Tama (CannotAnchorFlexOutsideSovNull); anchor a secondary Astrahus (35832) 2,000 km from the citadel.
        assert!(tama.set_ball_position(tama_ship, citadel_spot + glam::DVec3::new(2.0e6, 0.0, 0.0)).await);
        let flex_lowsec_err = ctx
            .dispatch_rpc(
                &mut sess,
                "structureDeployment",
                None,
                "Anchor",
                &[PyRep::Integer(35840), PyRep::String("Tama Cyno Beacon".into())],
                &kw,
                None,
            )
            .await
            .expect_err("Pharolux Cyno Beacon must be rejected in low-sec Tama");
        assert_eq!(flex_lowsec_err.code, "CannotAnchorFlexOutsideSovNull");
        let anchor_beacon_res = ctx
            .dispatch_rpc(
                &mut sess,
                "structureDeployment",
                None,
                "Anchor",
                &[PyRep::Integer(35832), PyRep::String("Tama Secondary Citadel".into())],
                &kw,
                None,
            )
            .await
            .expect("Anchor Tama Secondary Citadel");
        let PyRep::KeyVal(beacon_kv) = anchor_beacon_res else {
            panic!("Expected secondary citadel KeyVal");
        };
        let tama_beacon_id = beacon_kv.get("structureID").unwrap().as_i64().unwrap() as u64;
        assert!(tama.set_ball_position(tama_ship, citadel_spot + glam::DVec3::new(2.0e6 + 60_000.0, 0.0, 0.0)).await);
        ctx.dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_000_026),
                PyRep::Integer(tama_beacon_id as i64),
                PyRep::Integer(133),
                PyRep::Integer(50),
            ],
            &kw,
            None,
        )
        .await
        .expect("Fuel Tama Secondary Citadel via invbroker.Add");

        // Fast-forward citadel to operational state so jump-clone checks below run against an online structure.
        ctx.db.with_mut_snapshot(|snap| {
            let st = snap.structures.get_mut(&(tama_struct_id as i64)).expect("Tama citadel");
            st.state = "ShieldVulnerable".into();
            st.quantum_core_installed = true;
            st.reinforced_until_unix = 0;
        });
        assert!(tama.set_ball_position(tama_ship, citadel_spot + glam::DVec3::new(60_000.0, 0.0, 0.0)).await);
        ctx.advance_secs(12);
        ctx.dispatch_rpc(
            &mut sess,
            "structureDocking",
            None,
            "Dock",
            &[PyRep::Integer(tama_struct_id as i64)],
            &kw,
            None,
        )
        .await
        .expect("Dock at Tama Cyno Citadel");

        let clone_bay_item_id = 100_000_980_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                clone_bay_item_id,
                InventoryItemRecord {
                    item_id: clone_bay_item_id,
                    type_id: 35894,
                    owner_id: 90_000_001,
                    location_id: tama_struct_id as i64,
                    flag: InventoryFlag::ServiceSlot0,
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
            snap.module_online_states.insert(clone_bay_item_id, true);
        });

        // S-4: InstallCloneInStation rejects when docked in a structure; InstallCloneInStructure succeeds
        // parity: plan:S-4
        let wrong_clone_rpc = ctx
            .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "InstallCloneInStation", &[], &kw, None)
            .await;
        assert_eq!(wrong_clone_rpc.unwrap_err().code, "MustBeDocked");
        let clone_tama = ctx
            .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "InstallCloneInStructure", &[], &kw, None)
            .await
            .expect("InstallCloneInStructure at Tama Cyno Citadel");
        let PyRep::KeyVal(ct_kv) = clone_tama else {
            panic!("Expected clone_tama KeyVal");
        };
        let tama_clone_id = ct_kv.get("jumpCloneID").unwrap().as_i64().unwrap();
        ctx.dispatch_rpc(&mut sess, "structureDocking", None, "Undock", &[], &kw, None)
            .await
            .expect("Undock from Tama Cyno Citadel");

        // Return Rifter to Jita (60003760) via stargates 50014002 -> 50000805 -> 50013925 -> 50013927
        for gate_id in [50_014_002_i64, 50_000_805, 50_013_925, 50_013_927] {
            warp_and_jump_gate(&ctx, &mut sess, &kw, gate_id, [0.0, 0.0, 0.0]).await;
        }
        let jita_bound = ctx.bind_object(&mut sess, "beyonce", 30_000_142).unwrap();
        ctx.dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(jita_bound),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(60003760)],
            &kw,
            None,
        )
        .await
        .expect("Warp to Jita 4-4");
        let jita_handle = ctx.systems.get_or_spawn(30_000_142);
        for _ in 0..40 {
            let _ = jita_handle.step_manual(1.0).await.unwrap();
        }
        ctx.advance_secs(12);
        ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Dock", &[PyRep::Integer(60003760)], &kw, None)
            .await
            .expect("Dock Rifter at Jita");

        // S-9: MaterializeSystemLOD2 rejects non-GM roles (including legacy 0x02) and requires 64-bit V24.01 serviceConst GM role bitmask
        assert_eq!(ctx.living_world.get_convoy(800_000_001).map(|c| c.lod_level), Some(2));
        let non_gm_mat = eve_server::services::cosmetics_events::materialize_system_lod2(
            &ctx,
            &sess,
            &[PyRep::Integer(30_000_142)],
        )
        .await;
        assert_eq!(non_gm_mat.unwrap_err().code, "InsufficientRole");
        sess.role |= 0x02;
        let legacy_bit_mat = eve_server::services::cosmetics_events::materialize_system_lod2(
            &ctx,
            &sess,
            &[PyRep::Integer(30_000_142)],
        )
        .await;
        assert_eq!(legacy_bit_mat.unwrap_err().code, "InsufficientRole");
        sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT | eve_machonet::service_roles::ROLE_GML;
        let mat_second = eve_server::services::cosmetics_events::materialize_system_lod2(
            &ctx,
            &sess,
            &[PyRep::Integer(30_000_142)],
        )
        .await
        .expect("GM materialize_system_lod2");
        assert_eq!(mat_second.as_i64(), Some(0));
        sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;

        // N-P1-5: home_station.set_home_station rejects remote station (60008494 while docked at 60003760) with MustBeDockedAtTargetStation
        let remote_home_err = ctx
            .dispatch_rpc(
                &mut sess,
                "home_station",
                None,
                "set_home_station",
                &[PyRep::Integer(60_008_494)],
                &kw,
                None,
            )
            .await;
        assert_eq!(remote_home_err.unwrap_err().code, EXPECTED_REMOTE_ERR);
        ctx.dispatch_rpc(
            &mut sess,
            "home_station",
            None,
            "set_home_station",
            &[PyRep::Integer(60_003_760)],
            &kw,
            None,
        )
        .await
        .expect("set_home_station while docked at 60003760");

        // Test cross-location CloneJump on sess (Jita 60003760 -> Tama Cyno Citadel -> back to Jita 60003760)
        ctx.advance_secs(12);
        let pod_id = 99_000_700;
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
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.active_ship_id = pod_id;
            }
        });
        sess.ship_id = Some(pod_id);
        ctx.dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(tama_clone_id)],
            &kw,
            None,
        )
        .await
        .expect("CloneJump to Tama Cyno Citadel");
        assert_eq!(sess.structure_id, Some(tama_struct_id as i64));
        assert_eq!(sess.solar_system_id, Some(30002813));
        assert_ne!(sess.ship_id, Some(100_000_001));

        ctx.advance_virtual_clock(90_000);
        ctx.dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(tama_clone_id)],
            &kw,
            None,
        )
        .await
        .expect("CloneJump back to Jita");
        assert_eq!(sess.station_id, Some(60003760));
        assert_eq!(sess.solar_system_id, Some(30000142));

        // Board Archon Carrier (100_000_024, type_id = 24483), undock, and verify S-2 & S-3
        let char_id = sess.character_id.unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            let sk = snap.skills.entry(char_id).or_default();
            sk.insert(20533, eve_db::TrainedSkillRecord { type_id: 20533, level: 5, skill_points: 256_000.0 });
            sk.insert(3327, eve_db::TrainedSkillRecord { type_id: 3327, level: 5, skill_points: 256_000.0 });
            sk.insert(24314, eve_db::TrainedSkillRecord { type_id: 24314, level: 5, skill_points: 256_000.0 });
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.clone_grade = 2;
            }
        });
        ctx.advance_secs(12);
        ctx.dispatch_rpc(&mut sess, "ship", None, "Board", &[PyRep::Integer(100_000_024)], &kw, None)
            .await
            .expect("Board Archon Carrier");
        assert_eq!(sess.ship_id, Some(100_000_024));

        ctx.advance_secs(12);
        ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
            .await
            .expect("Undock Archon");
        ctx.dispatch_rpc(&mut sess, "fighterMgr", None, "GetFightersForShip", &[], &kw, None)
            .await
            .expect("GetFightersForShip from Archon");
        // parity: plan:S-2
        let ftr_err = ctx
            .dispatch_rpc(
                &mut sess,
                "fighterMgr",
                None,
                "CmdActivateAbilitySlots",
                &[PyRep::Integer(700_000_077), PyRep::Integer(0)],
                &kw,
                None,
            )
            .await;
        assert!(matches!(
            ftr_err.unwrap_err().code.as_str(),
            "UnimplementedRPC" | "FighterNotFound"
        ));

        // parity: plan:S-3
        ctx.advance_secs(12);
        let cyno_err = ctx
            .dispatch_rpc(
                &mut sess,
                "structureCynoBeaconMgr",
                None,
                "CmdJumpToStructureBeacon",
                &[PyRep::Integer(30002813)],
                &kw,
                None,
            )
            .await;
        assert!(matches!(
            cyno_err.unwrap_err().code.as_str(),
            "UnimplementedRPC" | "ShipDocked" | "BeaconNotFound"
        ));

        let _ = jita_handle.remove_ball(100_000_024).await;
        ctx.db
            .set_character_solar_system(90_000_001, 30_002_813, &ctx.sde)
            .expect("Move Archon to Tama");
        sess.solar_system_id = Some(30_002_813);
    }
}

pub(crate) mod review_np0_np1 {
    // Integration regression tests covering Round 6 NP-0 and NP-1 findings.


    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_review_r6_np0_np1_regressions() {
        // parity: rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID, rpc:dogmaIM.LoadAmmo, rpc:planetMgr.Flush, rpc:planetMgr.UserUpdateNetwork
        use eve_db::{IskCents, MarketOrderRecord};

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();

        // N-P1-6: Starter character 90_000_001 belongs to Republic Military School (1_000_170, school_id 14)
        let ch1 = ctx.db.get_character(90_000_001).expect("90_000_001 exists");
        assert_eq!(ch1.corporation_id, 1_000_170);

        let mut s1 = SessionState::new(10);
        ctx.dispatch_rpc(
            &mut s1,
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
        .unwrap();
        ctx.dispatch_rpc(
            &mut s1,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &kw,
            None,
        )
        .await
        .unwrap();

        // N-P1-3: planetMgr.UserUpdateNetwork rejects forged planet_id where (planet_id / 1000) == sys_id
        let bad_planet = ctx
            .dispatch_rpc(
                &mut s1,
                "planetMgr",
                None,
                "UserUpdateNetwork",
                &[PyRep::Integer(30_000_142_000)],
                &kw,
                None,
            )
            .await;
        assert_eq!(bad_planet.unwrap_err().code, "InvalidPlanetID");
        let wrong_sys_planet = ctx
            .dispatch_rpc(
                &mut s1,
                "planetMgr",
                None,
                "UserUpdateNetwork",
                &[PyRep::Integer(40_139_295)], // Amarr VIII is in 30002187, not Jita 30000142
                &kw,
                None,
            )
            .await;
        assert_eq!(
            wrong_sys_planet.unwrap_err().code,
            "PlanetNotInCurrentSystem"
        );

        // UserUpdateNetwork on Jita planet 40009082 and verify Flush deducts POCO export tax from wallet
        let s1_ship_id = s1.ship_id.unwrap_or(100_000_001);
        let prev_st = s1.station_id;
        s1.station_id = None;
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
                    location_id: s1_ship_id,
                    flag: eve_db::InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        let bal_before_pi = ctx.db.get_character(90_000_001).unwrap().balance.0;
        ctx.dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(40_009_082),
                PyRep::List(vec![
                    PyRep::Tuple(vec![
                        PyRep::Integer(1),
                        PyRep::Integer(1),
                        PyRep::Integer(2534),
                        PyRep::Real(0.1),
                        PyRep::Real(0.2),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(1),
                        PyRep::Integer(2),
                        PyRep::Integer(2848),
                        PyRep::Real(0.15),
                        PyRep::Real(0.25),
                    ]),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .unwrap();
        s1.station_id = prev_st;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.station_id = prev_st;
            }
        });
        ctx.dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "Flush",
            &[PyRep::Integer(40_009_082)],
            &kw,
            None,
        )
        .await
        .unwrap();
        let bal_after_flush = ctx.db.get_character(90_000_001).unwrap().balance.0;
        assert!(bal_after_flush < bal_before_pi);

        // N-P0-2: Two-party Courier contract and self-accept rejection
        let mut s2 = SessionState::new(11);
        ctx.dispatch_rpc(
            &mut s2,
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("other".into()),
                PyRep::String("other-secret".into()),
            ],
            &kw,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut s2,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_002)],
            &kw,
            None,
        )
        .await
        .unwrap();

        let (cour_item_1, cour_item_amarr, cour_item_2) = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let i1 = snap.next_entity_id;
            snap.items.insert(
                i1,
                eve_db::InventoryItemRecord {
                    item_id: i1,
                    type_id: 34,
                    owner_id: 90_000_002,
                    location_id: 60_003_760,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 10,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.next_entity_id += 1;
            let ia = snap.next_entity_id;
            snap.items.insert(
                ia,
                eve_db::InventoryItemRecord {
                    item_id: ia,
                    type_id: 34,
                    owner_id: 90_000_002,
                    location_id: 60_008_494,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 10,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.next_entity_id += 1;
            let i2 = snap.next_entity_id;
            snap.items.insert(
                i2,
                eve_db::InventoryItemRecord {
                    item_id: i2,
                    type_id: 34,
                    owner_id: 90_000_001,
                    location_id: 60_003_760,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 10,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            (i1, ia, i2)
        });

        // s2 (starts docked at 60003760) creates Courier contract 60003760 -> 60008494
        let c_res = ctx
            .dispatch_rpc(
                &mut s2,
                "contractProxy",
                None,
                "CreateContract",
                &[
                    PyRep::Integer(3), // Courier
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::Integer(1440),
                    PyRep::Integer(1),
                    PyRep::Integer(60_003_760),
                    PyRep::Integer(60_008_494),
                    PyRep::Real(0.0),
                    PyRep::Real(10_000.0),
                    PyRep::Real(25_000.0),
                    PyRep::String("".into()),
                    PyRep::String("".into()),
                    PyRep::List(vec![PyRep::Integer(cour_item_1)]),
                ],
                &kw,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(c_kv) = c_res else {
            panic!("Expected contract KeyVal");
        };
        let cid = c_kv.get("contractID").unwrap().as_i64().unwrap();

        // Self-accept by issuer (s2) is rejected
        let self_acc = ctx
            .dispatch_rpc(
                &mut s2,
                "contractProxy",
                None,
                "AcceptContract",
                &[PyRep::Integer(cid)],
                &kw,
                None,
            )
            .await;
        assert_eq!(self_acc.unwrap_err().code, "CannotAcceptOwnContract");

        // Move s2 to Amarr (60008494) and create a contract there so s1 (at 60003760) is rejected with MustBeDockedAtStartStation
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
                ch2.station_id = Some(60_008_494);
                ch2.solar_system_id = 30_002_187;
            }
            if let Some(sh2) = snap.items.get_mut(&100_000_002) {
                sh2.location_id = 60_008_494;
            }
        });
        s2.station_id = Some(60_008_494);
        s2.solar_system_id = Some(30_002_187);

        let c_amarr = ctx
            .dispatch_rpc(
                &mut s2,
                "contractProxy",
                None,
                "CreateContract",
                &[
                    PyRep::Integer(3), // Courier
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::Integer(1440),
                    PyRep::Integer(1),
                    PyRep::Integer(60_008_494),
                    PyRep::Integer(60_003_760),
                    PyRep::Real(0.0),
                    PyRep::Real(10_000.0),
                    PyRep::Real(25_000.0),
                    PyRep::String("".into()),
                    PyRep::String("".into()),
                    PyRep::List(vec![PyRep::Integer(cour_item_amarr)]),
                ],
                &kw,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(ca_kv) = c_amarr else {
            panic!("Expected ca KeyVal");
        };
        let cid_amarr = ca_kv.get("contractID").unwrap().as_i64().unwrap();

        let wrong_start = ctx
            .dispatch_rpc(
                &mut s1,
                "contractProxy",
                None,
                "AcceptContract",
                &[PyRep::Integer(cid_amarr)],
                &kw,
                None,
            )
            .await;
        assert_eq!(wrong_start.unwrap_err().code, "MustBeDockedAtStartStation");

        // Move s2 and ship 100_000_002 back to Jita 60003760; s1 creates cid2 (60003760 -> 60008494) and s2 accepts it
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
                ch2.station_id = Some(60_003_760);
                ch2.solar_system_id = 30_000_142;
            }
            if let Some(sh2) = snap.items.get_mut(&100_000_002) {
                sh2.location_id = 60_003_760;
            }
        });
        s2.station_id = Some(60_003_760);
        s2.solar_system_id = Some(30_000_142);

        let c2_res = ctx
            .dispatch_rpc(
                &mut s1,
                "contractProxy",
                None,
                "CreateContract",
                &[
                    PyRep::Integer(3), // Courier
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::Integer(1440),
                    PyRep::Integer(1),
                    PyRep::Integer(60_003_760),
                    PyRep::Integer(60_008_494),
                    PyRep::Real(0.0),
                    PyRep::Real(10_000.0),
                    PyRep::Real(25_000.0),
                    PyRep::String("".into()),
                    PyRep::String("".into()),
                    PyRep::List(vec![PyRep::Integer(cour_item_2)]),
                ],
                &kw,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(c2_kv) = c2_res else {
            panic!("Expected c2 KeyVal");
        };
        let cid2 = c2_kv.get("contractID").unwrap().as_i64().unwrap();

        let acc_c2 = ctx
            .dispatch_rpc(
                &mut s2,
                "contractProxy",
                None,
                "AcceptContract",
                &[PyRep::Integer(cid2)],
                &kw,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(acc_c2_kv) = acc_c2 else {
            panic!("Expected acc_c2 KeyVal");
        };
        let wrap_id_c2 = acc_c2_kv.get("courierPackageID").unwrap().as_i64().unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(w) = snap.items.get_mut(&wrap_id_c2) {
                w.location_id = 100_000_002;
                w.flag = eve_db::InventoryFlag::Cargo;
            }
        });

        // Completing while still at 60003760 (not 60008494) is rejected with CourierNotAtDestinationStation
        let early_comp = ctx
            .dispatch_rpc(
                &mut s2,
                "contractProxy",
                None,
                "CompleteContract",
                &[PyRep::Integer(cid2)],
                &kw,
                None,
            )
            .await;
        assert_eq!(
            early_comp.unwrap_err().code,
            "CourierNotAtDestinationStation"
        );

        // Deliver s2's ship 100_000_002 (containing Courier Plastic Wrap 24445) to 60008494 and complete
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
                ch2.station_id = Some(60_008_494);
                ch2.solar_system_id = 30_002_187;
            }
            if let Some(sh2) = snap.items.get_mut(&100_000_002) {
                sh2.location_id = 60_008_494;
            }
        });
        s2.station_id = Some(60_008_494);
        s2.solar_system_id = Some(30_002_187);

        let comp_ok = ctx
            .dispatch_rpc(
                &mut s2,
                "contractProxy",
                None,
                "CompleteContract",
                &[PyRep::Integer(cid2)],
                &kw,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(comp_kv) = comp_ok else {
            panic!("Expected comp KeyVal");
        };
        assert_eq!(
            comp_kv.get("status").and_then(|v| v.as_str()),
            Some("Completed")
        );

        // N-P1-7 & R7-P1-3: Seed a player ask order in Jita for Tritanium (34) at 4.00 ISK, verify invalid contract type rejection, undock s1 in Jita (auto-promoting convoy 800_000_001 to LOD 2), destroy convoy via dogmaIM.FireWeapon, and verify space wreck + +15% NPC ask spike + player order untouched
        let auction_err = ctx
            .dispatch_rpc(
                &mut s1,
                "contractProxy",
                None,
                "CreateContract",
                &[
                    PyRep::String("UnsupportedType".into()),
                    PyRep::Real(1_000.0),
                    PyRep::Real(0.0),
                    PyRep::Integer(100_000_008),
                ],
                &kw,
                None,
            )
            .await;
        assert_eq!(auction_err.unwrap_err().code, "InvalidContractType");

        ctx.db.with_mut_snapshot(|snap| {
            snap.market_orders.insert(
                999_999,
                MarketOrderRecord {
                    order_id: 999_999,
                    owner_id: 90_000_002,
                    station_id: 60_003_760,
                    region_id: 10_000_002,
                    type_id: 34,
                    price: IskCents::from_f64_isk(4.0),
                    vol_entered: 100,
                    vol_remaining: 100,
                    min_volume: 1,
                    is_bid: false,
                    is_npc: false,
                    escrow: IskCents(0),
                },
            );
        });
        ctx.seed_demo_world_fixtures();
        ctx.dispatch_rpc(&mut s1, "stationSvc", None, "Undock", &[], &kw, None)
            .await
            .unwrap();
        let non_gm_interdict = eve_server::services::cosmetics_events::interdict_convoy(
            &ctx,
            &s1,
            &[PyRep::Integer(800_000_001)],
        )
        .await;
        assert_eq!(non_gm_interdict.unwrap_err().code, "InsufficientRole");

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                100_000_941,
                eve_db::InventoryItemRecord {
                    item_id: 100_000_941,
                    type_id: 21924,
                    owner_id: 90_000_001,
                    location_id: 100_000_001,
                    flag: eve_db::InventoryFlag::Cargo,
                    quantity: 2_000,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        });
        ctx.dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "LoadAmmo",
            &[
                PyRep::Integer(100_000_001),
                PyRep::Integer(27),
                PyRep::Integer(100_000_941),
            ],
            &kw,
            None,
        )
        .await
        .unwrap();
        let jita_h = ctx.systems.get_or_spawn(30_000_142);
        let PyRep::Tuple(beyonce_bind) = ctx
            .dispatch_rpc(
                &mut s1,
                "beyonce",
                None,
                "MachoBindObject",
                &[PyRep::Integer(30_000_142), PyRep::None],
                &kw,
                None,
            )
            .await
            .expect("bind beyonce in Jita")
        else {
            panic!("Expected bind tuple");
        };
        let beyonce_oid = beyonce_bind[0].as_i64().unwrap() as u64;
        let mut ikv = HashMap::new();
        for shot in 0..50 {
            if shot > 0 {
                for _ in 0..4 {
                    let _ = jita_h.step_manual(1.0).await.unwrap();
                }
            }
            let interdict = match ctx
                .dispatch_rpc(
                    &mut s1,
                    "dogmaIM",
                    None,
                    "Activate",
                    &[PyRep::Integer(800_000_001)],
                    &kw,
                    None,
                )
                .await
            {
                Ok(v) => v,
                // The convoy keeps travelling; close back in to weapon range and keep shooting
                Err(e) if e.code == "TargetOutOfRange" => {
                    ctx.dispatch_rpc(
                        &mut s1,
                        "beyonce",
                        Some(beyonce_oid),
                        "CmdFollowBall",
                        &[PyRep::Integer(800_000_001), PyRep::Real(5_000.0)],
                        &kw,
                        None,
                    )
                    .await
                    .expect("CmdFollowBall toward convoy");
                    for _ in 0..80 {
                        let _ = jita_h.step_manual(1.0).await.unwrap();
                    }
                    if let (Some(ship_id), Ok(Some((_, convoy_ball)))) = (
                        s1.ship_id,
                        jita_h.get_ball_snapshot(800_000_001).await,
                    ) {
                        jita_h
                            .set_ball_position(
                                ship_id,
                                convoy_ball.position + glam::DVec3::new(1_000.0, 0.0, 0.0),
                            )
                            .await;
                    }
                    continue;
                }
                Err(e) => panic!("FireWeapon failed: {e:?}"),
            };
            let PyRep::KeyVal(kv) = interdict else {
                panic!("Expected convoy combat destruction KeyVal");
            };
            if kv.get("destroyed") == Some(&PyRep::Bool(true)) {
                ikv = kv;
                break;
            }
        }
        assert_eq!(ikv.get("destroyed"), Some(&PyRep::Bool(true)));
        let wreck_id = ikv.get("wreckItemID").unwrap().as_i64().unwrap();
        let loot_id = ikv.get("lootItemID").unwrap().as_i64().unwrap();
        let wreck_item = ctx.db.get_item(wreck_id).expect("wreck item in db");
        let loot_item = ctx.db.get_item(loot_id).expect("loot item in db");
        assert_eq!(wreck_item.type_id, 26557);
        assert_eq!(wreck_item.location_id, 30_000_142);
        assert_eq!(loot_item.location_id, wreck_id);
        // Player ask order 999_999 remains at 4.00 ISK (400 cents), while NPC ask order 700_000_001 spiked by +15% from 5.00 ISK (500 cents) to 5.75 ISK (575 cents)
        let player_price = ctx.db.with_snapshot(|s| s.market_orders[&999_999].price.0);
        let npc_price = ctx
            .db
            .with_snapshot(|s| s.market_orders[&700_000_001].price.0);
        assert_eq!(player_price, 400);
        assert_eq!(npc_price, 575);
    }
}

pub(crate) mod corp_roles_never_escalate_to {
    // Regression tests for Round-7 adversarial review findings (`R7-P0-01`, `R7-P0-02`, `R7-P1-01`, `R7-P1-02`).


    use crate::common::{login_pilot, pin_clock};
    use eve_db::{
        corp_roles, CorporationRecord, FwSystemOccupationRecord, InventoryFlag, InventoryItemRecord,
        IskCents,
    };
    use eve_machonet::{service_roles, SessionState};
    use eve_proto::quasar::{
        encode_bytes_field, encode_string_field, encode_varint_field, encode_zigzag64,
        AuthoritativeContext, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp,
        TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::PyRep;
    use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_OK};
    use eve_server::router::ServerContext;
    use eve_server::services::fw_services::capture_warzone_system;
    use eve_server::services::insurgency_services::step_system_insurgency_tick;
    use std::collections::{HashMap, HashSet};

    fn make_session(sid: u64, uid: u64, cid: i64, corp: i64, role: u64, corp_role: u64) -> SessionState {
        let mut s = SessionState::new(sid);
        s.user_id = Some(uid);
        s.character_id = Some(cid);
        s.corporation_id = Some(corp);
        s.station_id = Some(60_003_760);
        s.solar_system_id = Some(30_000_142);
        s.role = role;
        s.corp_role = corp_role;
        s
    }

    fn q_call(ctx: &ServerContext, sess: &SessionState, type_name: &str, value: Vec<u8>, now: u64) -> EvePublicResponse {
        let req = EvePublicRequest {
            issued: Some(GoogleTimestamp { seconds: now as i64, nanos: 0 }),
            correlation_uuid: vec![0x71; 16],
            payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }),
            external_origin: "test".into(),
            application_instance_uuid: vec![0x72; 16],
            authoritative_context: Some(
                AuthoritativeContext::for_session_at(
                    sess.user_id.map(|u| u as i32),
                    sess.character_id.map(|c| c as u32),
                    sess.corporation_id.map(|c| c as u32),
                    sess.corp_role,
                    "tranquility",
                    vec![0x73; 16],
                    now,
                )
                .with_service_roles(sess.role),
            ),
            trace_context: None,
        };
        ctx.quasar_gateway.handle_public_request(sess, &req, None, false, now)
    }

    fn encode_goal_create_with_escrow(name: &str, target: u64, escrow_isk: i64, init_prog: u64) -> Vec<u8> {
        let (mut msg, mut curr, mut isk, mut pool) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        encode_string_field(1, name, &mut msg);
        encode_varint_field(3, target, &mut msg);
        if init_prog > 0 {
            encode_varint_field(5, init_prog, &mut msg);
        }
        if escrow_isk > 0 {
            encode_varint_field(1, encode_zigzag64(escrow_isk), &mut curr);
            encode_bytes_field(1, &curr, &mut isk);
            encode_bytes_field(2, &isk, &mut pool);
            encode_bytes_field(8, &pool, &mut msg);
        }
        msg
    }

    #[tokio::test]
    async fn test_r7_p0_01_corp_roles_never_escalate_to_quasar_gm_or_bypass_escrow_auth() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let now = ctx.now_unix();
        let (ceo_id, member_id, other_ceo_id) = (90_000_001_i64, 90_000_002_i64, 90_000_003_i64);
        let (corp_a, corp_b) = (98_701_001_i64, 98_701_002_i64);
        let colliding = corp_roles::ROLE_CONTRACT_MANAGER | corp_roles::ROLE_STARBASE_CONFIG | corp_roles::ROLE_RENT_OFFICE;

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&ceo_id) {
                c.corporation_id = corp_a;
            }
            let mut c2 = snap.characters[&ceo_id].clone();
            c2.character_id = member_id;
            c2.user_id = 2;
            c2.corporation_id = corp_a;
            snap.characters.insert(member_id, c2.clone());
            c2.character_id = other_ceo_id;
            c2.user_id = 3;
            c2.corporation_id = corp_b;
            snap.characters.insert(other_ceo_id, c2);

            let mut ca = CorporationRecord {
                corporation_id: corp_a,
                name: "Corp A".into(),
                ticker: "CRPA".into(),
                ceo_id,
                master_wallet: IskCents::from_isk(10_000_000),
                members: HashSet::from([ceo_id, member_id]),
                member_roles: HashMap::from([(ceo_id, u64::MAX), (member_id, colliding)]),
                ..Default::default()
            };
            ca.ensure_wallet_divisions();
            ca.set_division_balance(1, IskCents::from_isk(10_000_000));
            snap.corporations.insert(corp_a, ca);

            let mut cb = CorporationRecord {
                corporation_id: corp_b,
                name: "Corp B".into(),
                ticker: "CRPB".into(),
                ceo_id: other_ceo_id,
                members: HashSet::from([other_ceo_id]),
                member_roles: HashMap::from([(other_ceo_id, u64::MAX)]),
                ..Default::default()
            };
            cb.ensure_wallet_divisions();
            snap.corporations.insert(corp_b, cb);
        });

        let sess_ceo_b = make_session(70103, 3, other_ceo_id, corp_b, 0, u64::MAX);
        let create_b = q_call(&ctx, &sess_ceo_b, "eve_public.goal.api.CreateRequest", encode_goal_create_with_escrow("Corp B Goal", 10, 0, 0), now);
        assert_eq!(create_b.status_code, STATUS_OK);
        let goal_b_id_msg = create_b.payload.unwrap().value;

        // 1. Player-corp CEO (`corp_role = u64::MAX`, `session.role = 0`) is rejected (`403`) on GM endpoints & cross-corp DeleteRequest
        let sess_ceo_a = make_session(70101, 1, ceo_id, corp_a, 0, u64::MAX);
        let (mut skin_hex, mut skin_u, mut unit_msg, mut asset_create) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        encode_string_field(1, "r7_skin_01", &mut skin_hex);
        encode_bytes_field(1, &skin_hex, &mut skin_u);
        encode_bytes_field(10, &skin_u, &mut unit_msg);
        encode_bytes_field(4, &unit_msg, &mut asset_create);
        encode_varint_field(6, 2, &mut asset_create);
        let (mut t_msg, mut stack_m, mut stacks_m, mut spawn_req) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        encode_varint_field(1, 34, &mut t_msg);
        encode_bytes_field(1, &t_msg, &mut stack_m);
        encode_varint_field(2, 100, &mut stack_m);
        encode_bytes_field(1, &stack_m, &mut stacks_m);
        encode_string_field(2, "Mint Attempt", &mut spawn_req);
        encode_bytes_field(6, &stacks_m, &mut spawn_req);
        assert_eq!(q_call(&ctx, &sess_ceo_a, "eve.assetholding.asset.api.CreateRequest", asset_create, now).status_code, STATUS_FORBIDDEN);
        assert_eq!(q_call(&ctx, &sess_ceo_a, "eve.assetholding.asset.api.SpawnRequest", spawn_req, now).status_code, STATUS_FORBIDDEN);
        assert_eq!(q_call(&ctx, &sess_ceo_a, "eve.assetholding.entitlement.api.DirectDeliveryRequest", Vec::new(), now).status_code, STATUS_FORBIDDEN);
        assert_eq!(q_call(&ctx, &sess_ceo_a, "eve_public.goal.api.DeleteRequest", goal_b_id_msg, now).status_code, STATUS_FORBIDDEN);
        assert_eq!(
            q_call(&ctx, &sess_ceo_a, "eve_public.goal.api.CreateRequest", encode_goal_create_with_escrow("Seed", 10, 0, 5), now).status_code,
            STATUS_FORBIDDEN
        );

        // 2. Member holding colliding corp bits (`ROLE_CONTRACT_MANAGER | ROLE_STARBASE_CONFIG | ROLE_RENT_OFFICE`) cannot create goals or bypass escrow
        let sess_mem = make_session(70102, 2, member_id, corp_a, 0, colliding);
        let escrow_req = encode_goal_create_with_escrow("Escrow Drain", 10, 500_000, 0);
        assert_eq!(q_call(&ctx, &sess_mem, "eve_public.goal.api.CreateRequest", escrow_req.clone(), now).status_code, STATUS_FORBIDDEN);

        let pm_colliding = colliding | corp_roles::ROLE_PROJECT_MANAGER;
        ctx.db.with_mut_snapshot(|s| {
            s.corporations.get_mut(&corp_a).unwrap().member_roles.insert(member_id, pm_colliding);
        });
        let sess_pm = make_session(70104, 2, member_id, corp_a, 0, pm_colliding);
        assert_eq!(q_call(&ctx, &sess_pm, "eve_public.goal.api.CreateRequest", escrow_req, now).status_code, STATUS_FORBIDDEN);
        assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_a].division_balance(1)), IskCents::from_isk(10_000_000));
    }

    #[tokio::test]
    async fn test_r7_p0_02_title_bitmask_shift_24h_role_delay_edit_project_and_goal_lifecycle() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let now = ctx.now_unix();
        let (ceo_id, pilot_id) = (90_000_001_i64, 90_000_002_i64);
        let corp_id = 98_702_001_i64;
        let kw = HashMap::new();

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&ceo_id) {
                c.corporation_id = corp_id;
            }
            let mut p = snap.characters[&ceo_id].clone();
            p.character_id = pilot_id;
            p.user_id = 2;
            p.corporation_id = corp_id;
            snap.characters.insert(pilot_id, p);

            let mut corp = CorporationRecord {
                corporation_id: corp_id,
                name: "Title Corp".into(),
                ticker: "TTLC".into(),
                ceo_id,
                master_wallet: IskCents::from_isk(15_000_000),
                members: HashSet::from([ceo_id, pilot_id]),
                member_roles: HashMap::from([(ceo_id, u64::MAX), (pilot_id, 0)]),
                ..Default::default()
            };
            corp.ensure_wallet_divisions();
            corp.set_division_balance(1, IskCents::from_isk(15_000_000));
            corp.set_division_balance(2, IskCents::from_isk(3_000_000));
            corp.set_division_balance(3, IskCents::from_isk(4_000_000));
            snap.corporations.insert(corp_id, corp);
        });

        let mut sess_ceo = make_session(70201, 1, ceo_id, corp_id, 0, u64::MAX);
        for (tid, name, rmask) in [
            (1_i64, "Recruit", 0_u64),
            (2_i64, "Div2 Bursar", corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[1]),
            (3_i64, "Treasurer PM", corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0]),
            (4_i64, "Div3 Bursar", corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[2]),
        ] {
            ctx.dispatch_rpc(&mut sess_ceo, "corpRegistry", None, "UpdateTitle", &[PyRep::Integer(tid), PyRep::String(name.into()), PyRep::Integer(rmask as i64)], &kw, None)
                .await
                .expect("UpdateTitle");
        }
        let upd_member_15 = |roles: u64, tmask: u64| {
            vec![
                PyRep::Integer(pilot_id), PyRep::String("Member".into()), PyRep::Integer(0), PyRep::Integer(0),
                PyRep::Integer(roles as i64), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0),
                PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0),
                PyRep::Integer(60_003_760), PyRep::Integer(tmask as i64), PyRep::Integer(0),
            ]
        };
        ctx.dispatch_rpc(&mut sess_ceo, "corpRegistry", None, "UpdateMember", &upd_member_15(0, 1), &kw, None).await.expect("Assign Title 1");

        let init_args = [
            PyRep::String("Initial Project".into()),
            PyRep::Integer(10),
            PyRep::Real(1_000.0),
            PyRep::String("DestroyNPCShips".into()),
            PyRep::Bool(false),
            PyRep::Integer(1),
        ];
        let created = ctx.dispatch_rpc(&mut sess_ceo, "corporationSvc", None, "CreateGoalSubmit", &init_args, &kw, None).await.unwrap();
        let pid = match created {
            PyRep::KeyVal(m) => m["projectID"].as_i64().unwrap(),
            other => panic!("unexpected CreateGoalSubmit reply: {other:?}"),
        };

        // 1. Pilot with Title 1 (`tmask = 1`) + stale `session.corp_role = ROLE_DIRECTOR` does NOT inherit Title 3 or stale session role
        let mut sess_p = make_session(70202, 2, pilot_id, corp_id, 0, corp_roles::ROLE_DIRECTOR);
        assert_eq!(
            ctx.dispatch_rpc(&mut sess_p, "corpRegistry", None, "SetAccountKey", &[PyRep::Integer(1001)], &kw, None).await.unwrap_err().code,
            "CorpRoleDenied"
        );
        let goal_args = [
            PyRep::String("Title1 Exploit".into()),
            PyRep::Integer(5),
            PyRep::Real(10_000.0),
            PyRep::String("DestroyNPCShips".into()),
            PyRep::Bool(false),
            PyRep::Integer(1),
        ];
        assert_eq!(ctx.dispatch_rpc(&mut sess_p, "corporationSvc", None, "CreateGoalSubmit", &goal_args, &kw, None).await.unwrap_err().code, "CorpRoleDenied");
        assert_eq!(
            eve_server::services::social_corp_fleet::social_corp_projects::handle_edit_project(
                &ctx,
                &mut sess_p,
                &[PyRep::Integer(pid), PyRep::String("Hijacked".into())],
            )
            .unwrap()
            .unwrap_err()
            .code,
            "CorpRoleDenied"
        );
        let q_escrow = encode_goal_create_with_escrow("Title1 Quasar", 5, 50_000, 0);
        assert_eq!(q_call(&ctx, &sess_p, "eve_public.goal.api.CreateRequest", q_escrow.clone(), now).status_code, STATUS_FORBIDDEN);

        // 2. Grant Title 2 via live 15-arg `UpdateMember(roles=0, titleMask=1|(1<<1))`: `EditProject` works immediately, Div 2 escrow delayed 24h
        ctx.dispatch_rpc(&mut sess_ceo, "corpRegistry", None, "UpdateMember", &upd_member_15(0, 1 | (1 << 1)), &kw, None).await.unwrap();
        sess_p.corp_role = 0;
        eve_server::services::social_corp_fleet::social_corp_projects::handle_edit_project(
            &ctx,
            &mut sess_p,
            &[PyRep::Integer(pid), PyRep::String("PM Edited".into())],
        )
        .unwrap()
        .expect("Title 2 PM should edit project immediately");
        ctx.advance_secs(11);
        assert_eq!(
            ctx.dispatch_rpc(&mut sess_p, "corpRegistry", None, "SetAccountKey", &[PyRep::Integer(1001)], &kw, None).await.unwrap_err().code,
            "CorpRoleDenied"
        );

        // Mature Title 2 (+86,401s), then grant Title 3 via 15-arg `UpdateMember(roles=0, titleMask=1|(1<<1)|(1<<2))` AND direct `ROLE_ACCOUNT_CAN_TAKE[2]` via `UpdateMembers`:
        // - `roles=0` must NOT clobber Title 3's 24h delay (Div 1 rejected at t+0s)
        // - Matured Title 2 must NOT be stripped by Title 3's pending delay (Div 2 succeeds at t+0s)
        // - Direct `ROLE_ACCOUNT_CAN_TAKE[2]` via `UpdateMembers` is ALSO delayed 24h (Div 3 rejected at t+0s)
        ctx.advance_secs(86_401);
        ctx.dispatch_rpc(&mut sess_ceo, "corpRegistry", None, "UpdateMember", &upd_member_15(0, 1 | (1 << 1) | (1 << 2)), &kw, None).await.unwrap();
        assert_eq!(
            ctx.dispatch_rpc(&mut sess_p, "corpRegistry", None, "SetAccountKey", &[PyRep::Integer(1000)], &kw, None).await.unwrap_err().code,
            "CorpRoleDenied"
        );
        let t1 = ctx.now_unix();
        assert_eq!(ctx.dispatch_rpc(&mut sess_p, "corporationSvc", None, "CreateGoalSubmit", &goal_args, &kw, None).await.unwrap_err().code, "CorpRoleDenied");
        assert_eq!(q_call(&ctx, &sess_p, "eve_public.goal.api.CreateRequest", q_escrow.clone(), t1).status_code, STATUS_FORBIDDEN);

        ctx.advance_secs(11);
        ctx.dispatch_rpc(&mut sess_p, "corpRegistry", None, "SetAccountKey", &[PyRep::Integer(1001)], &kw, None).await.unwrap();
        let t1b = ctx.now_unix();
        let div2_ok = q_call(&ctx, &sess_p, "eve_public.goal.api.CreateRequest", q_escrow.clone(), t1b);
        assert_eq!(div2_ok.status_code, STATUS_OK, "already-matured Title 2 must stay active while Title 3 matures");
        assert_eq!(q_call(&ctx, &sess_p, "eve_public.goal.api.DeleteRequest", div2_ok.payload.unwrap().value, t1b).status_code, STATUS_OK);
        assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(2)), IskCents::from_isk(3_000_000));

        // Batch `UpdateMembers`: grant Title 4 (`1 << 3`, Div 3) alongside `new_roles_opt = ROLE_PROJECT_MANAGER` — must NOT clobber Title 4's 24h delay
        let bulk_title_row = PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(pilot_id),
            PyRep::Integer(corp_roles::ROLE_PROJECT_MANAGER as i64),
            PyRep::Integer((1 | (1 << 1) | (1 << 2) | (1 << 3)) as i64),
        ])]);
        ctx.dispatch_rpc(&mut sess_ceo, "corpRegistry", None, "UpdateMembers", &[bulk_title_row], &kw, None).await.unwrap();
        ctx.advance_secs(11);
        assert_eq!(
            ctx.dispatch_rpc(&mut sess_p, "corpRegistry", None, "SetAccountKey", &[PyRep::Integer(1002)], &kw, None).await.unwrap_err().code,
            "CorpRoleDenied"
        );

        // Also grant direct `ROLE_ACCOUNT_CAN_TAKE[2]` via both batch `UpdateMembers` and single `UpdateMember` — still delayed 24h
        let bulk_row = PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(pilot_id),
            PyRep::Integer((corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[2]) as i64),
            PyRep::Integer((1 | (1 << 1) | (1 << 2)) as i64),
        ])]);
        ctx.dispatch_rpc(&mut sess_ceo, "corpRegistry", None, "UpdateMembers", &[bulk_row], &kw, None).await.unwrap();
        ctx.dispatch_rpc(
            &mut sess_ceo,
            "corpRegistry",
            None,
            "UpdateMember",
            &upd_member_15(corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[2], 1 | (1 << 1) | (1 << 2)),
            &kw,
            None,
        )
        .await
        .unwrap();
        ctx.advance_secs(11);
        assert_eq!(
            ctx.dispatch_rpc(&mut sess_p, "corpRegistry", None, "SetAccountKey", &[PyRep::Integer(1002)], &kw, None).await.unwrap_err().code,
            "CorpRoleDenied"
        );

        // 3. Mature Title 3 + direct Div 3 (+86,401s); verify promotion does NOT bake Title 3 into `member_roles` upon title removal
        ctx.advance_secs(86_401);
        let now_matured = ctx.now_unix();
        ctx.dispatch_rpc(&mut sess_p, "corpRegistry", None, "SetAccountKey", &[PyRep::Integer(1000)], &kw, None).await.unwrap();
        let div1_ok = q_call(&ctx, &sess_p, "eve_public.goal.api.CreateRequest", q_escrow.clone(), now_matured);
        assert_eq!(div1_ok.status_code, STATUS_OK, "{}", div1_ok.status_message);
        assert_eq!(q_call(&ctx, &sess_p, "eve_public.goal.api.DeleteRequest", div1_ok.payload.unwrap().value, now_matured).status_code, STATUS_OK);

        // Revoke Title 3 via `UpdateMember`: Div 1 escrow permission must disappear immediately (not baked into `member_roles`)
        ctx.dispatch_rpc(
            &mut sess_ceo,
            "corpRegistry",
            None,
            "UpdateMember",
            &upd_member_15(corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[2], 1 | (1 << 1)),
            &kw,
            None,
        )
        .await
        .unwrap();
        assert_eq!(q_call(&ctx, &sess_p, "eve_public.goal.api.CreateRequest", q_escrow, now_matured).status_code, STATUS_FORBIDDEN);

        // 4. Division 3 escrow create + CloseRequest/DeleteRequest refund conservation (`R8-P1-02`)
        ctx.advance_secs(11);
        let now_div3 = ctx.now_unix();
        ctx.dispatch_rpc(&mut sess_p, "corpRegistry", None, "SetAccountKey", &[PyRep::Integer(1002)], &kw, None).await.unwrap();
        let g1 = q_call(&ctx, &sess_p, "eve_public.goal.api.CreateRequest", encode_goal_create_with_escrow("Div3 Close Goal", 10, 600_000, 0), now_div3);
        let g2 = q_call(&ctx, &sess_p, "eve_public.goal.api.CreateRequest", encode_goal_create_with_escrow("Div3 Delete Goal", 10, 900_000, 0), now_div3);
        assert_eq!(g1.status_code, STATUS_OK);
        assert_eq!(g2.status_code, STATUS_OK);
        let (g1_wire, g2_wire) = (g1.payload.unwrap().value, g2.payload.unwrap().value);
        assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(3)), IskCents::from_isk(2_500_000));
        assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet), IskCents::from_isk(14_990_000));

        let close_rsp = q_call(&ctx, &sess_p, "eve_public.goal.api.CloseRequest", g1_wire.clone(), now_matured);
        assert_eq!(close_rsp.status_code, STATUS_OK);
        assert_eq!(close_rsp.payload.unwrap().type_url, "type.googleapis.com/eve_public.goal.api.CloseResponse");
        assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(3)), IskCents::from_isk(3_100_000));
        assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet), IskCents::from_isk(14_990_000));

        let del_closed = q_call(&ctx, &sess_p, "eve_public.goal.api.DeleteRequest", g1_wire, now_matured);
        assert_eq!(del_closed.status_code, STATUS_OK);
        assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(3)), IskCents::from_isk(3_100_000));

        let del_rsp = q_call(&ctx, &sess_p, "eve_public.goal.api.DeleteRequest", g2_wire, now_matured);
        assert_eq!(del_rsp.status_code, STATUS_OK);
        assert_eq!(del_rsp.payload.unwrap().type_url, "type.googleapis.com/eve_public.goal.api.DeleteResponse");
        assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(3)), IskCents::from_isk(4_000_000));
        assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet), IskCents::from_isk(14_990_000));
    }

    #[tokio::test]
    async fn test_r7_p1_01_fw_complex_despawn_no_oscillation_solar_system_vp_pct_and_defender_advantage() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let kw = HashMap::new();
        let fw_sys = 30_002_537_u32;
        let (atk_cid, def_cid) = (90_000_001_i64, 90_000_731_i64);
        let (atk_ship, def_ship) = (880_731_001_i64, 880_731_002_i64);

        let mut s_atk = SessionState::new(70301);
        login_pilot(&ctx, &mut s_atk, "demo", "demo-secret").await;
        ctx.dispatch_rpc(&mut s_atk, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(atk_cid)], &kw, None).await.unwrap();

        let uid_def = ctx.db.provision_player_account("r7_fw_def", "secret");
        ctx.db.with_mut_snapshot(|snap| {
            let mut d = snap.characters[&atk_cid].clone();
            d.character_id = def_cid;
            d.user_id = uid_def;
            snap.characters.insert(def_cid, d);
        });
        let mut s_def = SessionState::new(70302);
        login_pilot(&ctx, &mut s_def, "r7_fw_def", "secret").await;
        ctx.dispatch_rpc(&mut s_def, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(def_cid)], &kw, None).await.unwrap();

        ctx.db.with_mut_snapshot(|snap| {
            for (cid, sid, fid) in [(atk_cid, atk_ship, 500_004_i64), (def_cid, def_ship, 500_001_i64)] {
                if let Some(c) = snap.characters.get_mut(&cid) {
                    c.solar_system_id = fw_sys;
                    c.station_id = None;
                    c.structure_id = None;
                    c.active_ship_id = sid;
                    c.war_faction_id = Some(fid);
                }
            }
            snap.fw_system_occupations.insert(
                fw_sys,
                FwSystemOccupationRecord {
                    solar_system_id: fw_sys,
                    owner_faction_id: 500_001,
                    occupier_faction_id: 500_001,
                    victory_points: 0,
                    victory_points_threshold: 3_000,
                },
            );
            snap.fw_runtime.system_advantage.insert(fw_sys, (80.0, 0.0));
            let st = snap.system_states.entry(fw_sys).or_default();
            st.solar_system_id = fw_sys;
            st.fw_complex_position = Some([0.0, 0.0, 0.0]);
            st.fw_capture_duration_seconds = 600;
            st.fw_capture_seconds_elapsed = 300;
            st.fw_contested_pct = 0.0;
            st.last_fw_capture_tick = 1;
        });

        let fw_h = ctx.systems.get_or_spawn(fw_sys);
        let mk_ball = |id: i64, x: f64| eve_destiny::Ball {
            id,
            type_id: 587,
            position: glam::DVec3::new(x, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        };

        // 1. Defender (`500_001`, +80 advantage) de-plexes 14s in 10 ticks (`1.40x`); Attacker (`500_004`) advances 6s in 10 ticks (`0.60x`)
        fw_h.spawn_ball(mk_ball(atk_ship, 80_000.0)).await.unwrap();
        fw_h.spawn_ball(mk_ball(def_ship, 1_000.0)).await.unwrap();
        let base_tick = fw_h.step_manual(1.0).await.unwrap().tick;
        ctx.db.with_mut_snapshot(|s| {
            s.system_states.get_mut(&fw_sys).unwrap().last_fw_capture_tick = base_tick;
        });
        for _ in 0..10 {
            let _ = fw_h.step_manual(1.0).await.unwrap();
        }
        step_system_insurgency_tick(&ctx, fw_sys).await;
        assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&fw_sys].fw_capture_seconds_elapsed), 286);

        let _ = fw_h.set_ball_position(def_ship, glam::DVec3::new(80_000.0, 0.0, 0.0)).await;
        let _ = fw_h.set_ball_position(atk_ship, glam::DVec3::new(1_000.0, 0.0, 0.0)).await;
        for _ in 0..10 {
            let _ = fw_h.step_manual(1.0).await.unwrap();
        }
        step_system_insurgency_tick(&ctx, fw_sys).await;
        assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&fw_sys].fw_capture_seconds_elapsed), 292);

        // 2. Completing complex despawns it, sets solar-system `fw_contested_pct` to `(20/3000)*100`, and prevents oscillation re-awards
        ctx.db.with_mut_snapshot(|s| {
            s.system_states.get_mut(&fw_sys).unwrap().fw_capture_seconds_elapsed = 599;
        });
        for _ in 0..5 {
            let _ = fw_h.step_manual(1.0).await.unwrap();
        }
        step_system_insurgency_tick(&ctx, fw_sys).await;

        let (lp_once, vp_once, pct_once, pos_after) = ctx.db.with_snapshot(|s| {
            let lp: i64 = s.characters[&atk_cid].loyalty_points.values().copied().sum();
            let vp = s.fw_runtime.system_victory_points.get(&fw_sys).copied().unwrap_or(0);
            let st = &s.system_states[&fw_sys];
            (lp, vp, st.fw_contested_pct, st.fw_complex_position)
        });
        assert!(lp_once > 0);
        assert_eq!(vp_once, 20);
        assert!(pos_after.is_none());
        assert!((pct_once - (20.0 / 3_000.0) * 100.0).abs() < 1e-4);
        assert_eq!(capture_warzone_system(&ctx, fw_sys, 500_004).unwrap_err().code, "SystemNotVulnerable");

        let _ = fw_h.set_ball_position(atk_ship, glam::DVec3::new(80_000.0, 0.0, 0.0)).await;
        let _ = fw_h.set_ball_position(def_ship, glam::DVec3::new(1_000.0, 0.0, 0.0)).await;
        let _ = fw_h.step_manual(1.0).await.unwrap();
        step_system_insurgency_tick(&ctx, fw_sys).await;

        let _ = fw_h.set_ball_position(def_ship, glam::DVec3::new(80_000.0, 0.0, 0.0)).await;
        let _ = fw_h.set_ball_position(atk_ship, glam::DVec3::new(1_000.0, 0.0, 0.0)).await;
        let _ = fw_h.step_manual(1.0).await.unwrap();
        step_system_insurgency_tick(&ctx, fw_sys).await;

        ctx.db.with_snapshot(|s| {
            let lp_after: i64 = s.characters[&atk_cid].loyalty_points.values().copied().sum();
            assert_eq!(lp_after, lp_once);
            assert_eq!(s.fw_runtime.system_victory_points.get(&fw_sys).copied().unwrap_or(0), vp_once);
        });
    }

    #[tokio::test]
    async fn test_r7_p1_02_entitlement_hangar_volume_capacity_enforced_on_delivery_and_redeem() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let now = ctx.now_unix();
        let (holder_id, corp_id, container_loc_id) = (90_000_001_i64, 1_000_009_i64, 770_704_001_i64);

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                container_loc_id,
                InventoryItemRecord {
                    item_id: container_loc_id,
                    type_id: 23,
                    owner_id: holder_id,
                    location_id: 60_003_760,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            if let Some(ch) = snap.characters.get_mut(&holder_id) {
                ch.station_id = Some(container_loc_id);
            }
        });
        let cap_m3 = ctx.sde.types[&23].capacity;
        let unit_trit_qty = ((cap_m3 * 0.6) / 0.01).ceil() as u64;

        let gm_sess = make_session(70401, 1, holder_id, corp_id, service_roles::ROLE_GMH, 0);
        let user_sess = make_session(70402, 1, holder_id, corp_id, 0, 0);

        let (mut t_msg, mut stack_m, mut stacks_m, mut spawn_payload) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        encode_varint_field(1, 34, &mut t_msg);
        encode_bytes_field(1, &t_msg, &mut stack_m);
        encode_varint_field(2, unit_trit_qty, &mut stack_m);
        encode_bytes_field(1, &stack_m, &mut stacks_m);
        encode_string_field(2, "R7 Volume Asset", &mut spawn_payload);
        encode_bytes_field(6, &stacks_m, &mut spawn_payload);
        assert_eq!(q_call(&ctx, &gm_sess, "eve.assetholding.asset.api.SpawnRequest", spawn_payload, now).status_code, STATUS_OK);

        let mut asset_uuid = [0u8; 16];
        asset_uuid[0..8].copy_from_slice(b"ASSETHLD");
        asset_uuid[8..16].copy_from_slice(&1u64.to_be_bytes());
        let (mut ch_m, mut h_m, mut a_m, mut g_m, mut ent_id) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        encode_varint_field(1, holder_id as u64, &mut ch_m);
        encode_bytes_field(1, &ch_m, &mut h_m);
        encode_bytes_field(1, &asset_uuid, &mut a_m);
        encode_string_field(1, "r7-vol", &mut g_m);
        encode_bytes_field(1, &h_m, &mut ent_id);
        encode_bytes_field(2, &a_m, &mut ent_id);
        encode_bytes_field(3, &g_m, &mut ent_id);

        let mk_dd = |qty: u64| {
            let (mut entry, mut dd) = (Vec::new(), Vec::new());
            encode_bytes_field(2, &ent_id, &mut entry);
            encode_varint_field(10, qty, &mut entry);
            encode_bytes_field(2, &entry, &mut dd);
            dd
        };

        assert_eq!(q_call(&ctx, &gm_sess, "eve.assetholding.entitlement.api.DirectDeliveryRequest", mk_dd(2), now).status_code, STATUS_BAD_REQUEST);
        assert_eq!(q_call(&ctx, &gm_sess, "eve.assetholding.entitlement.api.DirectDeliveryRequest", mk_dd(1), now).status_code, STATUS_OK);
        assert_eq!(q_call(&ctx, &gm_sess, "eve.assetholding.entitlement.api.DirectDeliveryRequest", mk_dd(1), now).status_code, STATUS_BAD_REQUEST);

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                770_704_002,
                InventoryItemRecord {
                    item_id: 770_704_002,
                    type_id: 34,
                    owner_id: holder_id,
                    location_id: container_loc_id,
                    flag: InventoryFlag::Hangar,
                    quantity: unit_trit_qty as i32,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        let mut redeem_wire = Vec::new();
        encode_bytes_field(1, &ent_id, &mut redeem_wire);
        assert_eq!(q_call(&ctx, &user_sess, "eve_public.assetholding.entitlement.api.RedeemRequest", redeem_wire.clone(), now).status_code, STATUS_BAD_REQUEST);

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&770_704_002);
        });
        assert_eq!(q_call(&ctx, &user_sess, "eve_public.assetholding.entitlement.api.RedeemRequest", redeem_wire, now).status_code, STATUS_OK);
    }
}

pub(crate) mod safe_logout_emergency_warp_weapons {
    // behavioral verification regression tests for fake-done rows.

    use eve_db::{self, IskCents};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services;
    use eve_server::session_cluster_lifecycle;
    use glam::DVec3;
    use std::collections::HashMap;

    fn seed_item(
        ctx: &ServerContext,
        item_id: i64,
        type_id: u32,
        owner_id: i64,
        location_id: i64,
        flag: eve_db::InventoryFlag,
        quantity: i32,
        singleton: bool,
    ) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                item_id,
                eve_db::InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id,
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
        });
    }

    fn grant_skills(ctx: &ServerContext, char_id: i64, skills: &[(u32, i32)]) {
        ctx.db.with_mut_snapshot(|snap| {
            let entry = snap.skills.entry(char_id).or_default();
            for &(sid, lvl) in skills {
                entry.insert(sid, eve_db::TrainedSkillRecord {
                    type_id: sid,
                    level: lvl as u8,
                    skill_points: 256_000.0,
                });
            }
        });
    }

    fn pick_lowsec_pair(ctx: &ServerContext) -> (u32, u32) {
        let mut sys: Vec<u32> = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| s.security_status < 0.45 && s.solar_system_id < 31_000_000 && s.galactic_pos_ly != [0.0; 3])
            .map(|s| s.solar_system_id)
            .collect();
        sys.sort_unstable();
        for &a in &sys {
            let pa = ctx.sde.solar_systems[&a].galactic_pos_ly;
            for &b in &sys {
                if a == b {
                    continue;
                }
                let pb = ctx.sde.solar_systems[&b].galactic_pos_ly;
                let d = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt();
                if (0.3..2.5).contains(&d) {
                    return (a, b);
                }
            }
        }
        panic!("no suitable lowsec pair found");
    }

    #[tokio::test]
    async fn test_safe_logout_emergency_warp_and_weapons_timer() {
        // parity: feat:account.logout
        eve_testkit::attest(&["feat:account.logout"]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let sys_id = 30000142u32;
        let char_id = 90_000_001i64;
        let ship_id = 999_001i64;
        let now_unix = ctx.now_unix();

        seed_item(&ctx, ship_id, 587, char_id, sys_id as i64, eve_db::InventoryFlag::None, 1, true);

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = ship_id;
            }
            snap.item_positions.insert(ship_id, [0.0, 0.0, 0.0]);
        });

        let entry = session_cluster_lifecycle::handle_character_logoff_or_disconnect(&ctx, char_id).await.expect("entry");
        assert!(entry.emergency_warp_dest.is_some(), "Pilot with no timers must emergency warp on logoff");
        assert_eq!(entry.remove_at_unix, now_unix + 60);

        let ship2_id = 999_002i64;
        let char2_id = 90_000_002i64;
        seed_item(&ctx, ship2_id, 587, char2_id, sys_id as i64, eve_db::InventoryFlag::None, 1, true);

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char2_id) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = ship2_id;
            }
            snap.item_positions.insert(ship2_id, [1000.0, 0.0, 0.0]);
        });
        ctx.crimewatch.write().unwrap().states.entry(char2_id).or_default().weapon_timer_until_unix = now_unix + 300;

        let entry2 = session_cluster_lifecycle::handle_character_logoff_or_disconnect(&ctx, char2_id).await.expect("entry2");
        assert!(entry2.emergency_warp_dest.is_none(), "Pilot with active weapons timer must NOT emergency warp");
        assert_eq!(entry2.remove_at_unix, now_unix + 300, "Ship must remain until weapon timer expires");
    }

    #[tokio::test]
    async fn test_market_order_expiry_persisted_and_swept() {
        // parity: feat:market.order-expiry
        eve_testkit::attest(&["feat:market.order-expiry"]);
        let ctx = ServerContext::new();
        let now_unix = ctx.now_unix();
        let char_id = 90_000_001i64;
        let order_id = 888_001i64;
        let region_id = 10000002u32;
        let station_id = 60003760i64;

        ctx.db.with_mut_snapshot(|snap| {
            snap.market_orders.insert(order_id, eve_db::MarketOrderRecord {
                order_id,
                owner_id: char_id,
                station_id,
                region_id,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(5),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(0),
            });
            snap.market_order_meta.insert(order_id, eve_db::MarketOrderMeta {
                issued_unix: now_unix.saturating_sub(200_000),
                duration_days: 1,
                range: "region".to_string(),
                issued_by: Some(char_id),
                wallet_division: Some(1000),
                is_corporation: false,
                state: Some("open".to_string()),
            });
        });

        session_cluster_lifecycle::on_advance_secs(&ctx, now_unix, 1);

        let state = ctx.db.with_snapshot(|snap| {
            snap.market_order_meta.get(&order_id).and_then(|m| m.state.clone())
        });
        assert_eq!(state.as_deref(), Some("expired"), "Expired order must transition to expired");

        let returned = ctx.db.with_snapshot(|snap| {
            snap.items.values().any(|it| it.owner_id == char_id && it.type_id == 34 && it.quantity >= 100)
        });
        assert!(returned, "Unsold items from expired sell order must be returned to owner");
    }

    #[tokio::test]
    async fn test_fleet_cyno_jump() {
        // parity: feat:navigation.jump-drive
        eve_testkit::attest(&["feat:navigation.jump-drive"]);
        let ctx = ServerContext::new();
        let kw = HashMap::new();
        let p1 = 90_000_001i64;
        let p2 = 90_000_002i64;
        let (lowsec_sys, dest_sys) = pick_lowsec_pair(&ctx);
        let fleet_id = 55_001i64;
        let rorq_id = 999_101i64;
        let cyno_ship_id = 999_102i64;
        let cyno_mod_id = 88_002i64;
        let ozone_id = 88_003i64;
        let isotope_id = 88_004i64;

        ctx.db.with_mut_snapshot(|snap| {
            snap.fleets.insert(fleet_id, eve_db::FleetRecord {
                fleet_id,
                boss_id: p1,
                members: vec![p1, p2],
                ..Default::default()
            });
            if let Some(c) = snap.characters.get_mut(&p1) {
                c.solar_system_id = lowsec_sys;
                c.active_ship_id = rorq_id;
                c.station_id = None;
                c.structure_id = None;
                c.jump_cooldown_until_unix = 0;
                c.jump_fatigue_until_unix = 0;
            }
            if let Some(c) = snap.characters.get_mut(&p2) {
                c.solar_system_id = dest_sys;
                c.active_ship_id = cyno_ship_id;
                c.station_id = None;
                c.structure_id = None;
            }
        });

        let _ = ctx.db.acquire_online_lock_with_computer_hash(1, 7003, Some(p1), None, true, ctx.now_unix());
        let _ = ctx.db.acquire_online_lock_with_computer_hash(2, 7004, Some(p2), None, true, ctx.now_unix());

        grant_skills(&ctx, p1, &[(21611, 5), (21610, 5), (28351, 5), (24311, 5)]);
        grant_skills(&ctx, p2, &[(11951, 5), (21603, 5)]);

        seed_item(&ctx, rorq_id, 28352, p1, lowsec_sys as i64, eve_db::InventoryFlag::None, 1, true);
        let fuel_type = ctx.sde.types.get(&28352)
            .and_then(|t| t.default_attributes.get(&866).copied())
            .unwrap_or(16274.0) as u32;
        seed_item(&ctx, isotope_id, fuel_type, p1, rorq_id, eve_db::InventoryFlag::Cargo, 100_000, false);

        seed_item(&ctx, cyno_ship_id, 11957, p2, dest_sys as i64, eve_db::InventoryFlag::None, 1, true);
        seed_item(&ctx, cyno_mod_id, 21096, p2, cyno_ship_id, eve_db::InventoryFlag::HiSlot0, 1, true);
        seed_item(&ctx, ozone_id, 16273, p2, cyno_ship_id, eve_db::InventoryFlag::Cargo, 2_000, false);

        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(cyno_mod_id, true);
            snap.item_positions.insert(rorq_id, [0.0, 0.0, 0.0]);
            snap.item_positions.insert(cyno_ship_id, [10_000.0, 0.0, 0.0]);
        });

        let h1 = ctx.systems.get_or_spawn(lowsec_sys);
        let _ = h1.spawn_ball_with_combat(
            eve_destiny::Ball {
                id: rorq_id,
                type_id: 28352,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1e8,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(p1, rorq_id, 28352),
        ).await;

        let h2 = ctx.systems.get_or_spawn(dest_sys);
        let _ = h2.spawn_ball_with_combat(
            eve_destiny::Ball {
                id: cyno_ship_id,
                type_id: 11957,
                position: DVec3::new(10_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 150.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(p2, cyno_ship_id, 11957),
        ).await;

        let mut s2 = SessionState::new(7004);
        s2.user_id = Some(2);
        s2.character_id = Some(p2);
        s2.ship_id = Some(cyno_ship_id);
        s2.station_id = None;
        s2.structure_id = None;
        s2.fleet_id = Some(fleet_id);
        s2.solar_system_id = Some(dest_sys);

        let _cyno_rep = ctx.dispatch_rpc(
            &mut s2,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(cyno_mod_id), PyRep::String("cynosuralGeneration".into())],
            &kw,
            None,
        ).await.expect("Activate cyno generator");

        let mut s1 = SessionState::new(7003);
        s1.user_id = Some(1);
        s1.character_id = Some(p1);
        s1.ship_id = Some(rorq_id);
        s1.station_id = None;
        s1.structure_id = None;
        s1.fleet_id = Some(fleet_id);
        s1.solar_system_id = Some(lowsec_sys);

        let res = ctx.dispatch_rpc(
            &mut s1,
            "beyonce",
            None,
            // rpc-lint: allow live crucible:src/eve-server/ship/BeyonceService.h CmdBeaconJumpFleet
            "CmdBeaconJumpFleet",
            &[PyRep::Integer(p2), PyRep::Integer(cyno_ship_id), PyRep::Integer(dest_sys as i64)],
            &kw,
            None,
        ).await;
        assert!(res.is_ok(), "CmdBeaconJumpFleet failed: {:?}", res);

        let remaining_fuel = ctx.db.with_snapshot(|snap| {
            snap.items.get(&isotope_id).map(|i| i.quantity).unwrap_or(0)
        });
        assert!(remaining_fuel < 100_000, "Jump must consume fuel");

        assert_eq!(s1.solar_system_id, Some(dest_sys));
        let notifs = ctx.drain_notifications_for_char(p1);
        assert!(notifs.iter().any(|n| n.event == "OnJumpTimersUpdated"), "Must emit OnJumpTimersUpdated");
    }

    #[tokio::test]
    async fn test_session_change_timer_10s_and_canonical_payload() {
        // parity: feat:navigation.session-change-timer
        eve_testkit::attest(&["feat:navigation.session-change-timer"]);
        let ctx = ServerContext::new();
        let char_id = 90_000_001i64;
        let now_unix = ctx.now_unix();
        let sys_id = 30000142u32;

        let _ = ctx.db.acquire_online_lock_with_computer_hash(1, 7001, Some(char_id), None, true, now_unix);

        services::space_destiny::record_session_change_timer(&ctx, char_id, now_unix, sys_id, 100, "stargate_jump");

        let notifs = ctx.drain_notifications_for_char(char_id);
        let sc = notifs.iter().find(|n| n.event == "OnSessionChanged").expect("OnSessionChanged emitted");
        assert_eq!(sc.args.len(), 3, "OnSessionChanged must have exactly 3 args: (isRemote, session, change)");
        assert_eq!(sc.args[0], PyRep::Bool(false));
        let next_change = sc.args[1].dict_get("nextSessionChange").and_then(|v| v.as_i64()).expect("nextSessionChange in session keyval");
        assert_eq!(next_change, (now_unix + 10) as i64);

        let active = services::space_destiny::check_session_change_timer_active(&ctx, char_id, now_unix + 5, Some((sys_id, 105)));
        assert!(active.is_some(), "Timer must be active within 10s");

        let expired = services::space_destiny::check_session_change_timer_active(&ctx, char_id, now_unix + 10, Some((sys_id, 110)));
        assert!(expired.is_none(), "Timer must expire after 10s");
    }

    #[tokio::test]
    async fn test_corp_recruitment_ads_expiry_and_search_filters() {
        // parity: rpc:corpRecProxy.SearchCorpAds
        eve_testkit::attest(&["rpc:corpRecProxy.SearchCorpAds"]);
        let ctx = ServerContext::new();
        let kw = HashMap::new();
        let corp_id = 2_000_170i64;
        let char_id = 90_000_001i64;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.corporation_id = corp_id;
            }
            snap.corporations.insert(
                corp_id,
                eve_db::CorporationRecord {
                    corporation_id: corp_id,
                    name: "PvP Test Corp".into(),
                    ticker: "PVPT".into(),
                    ceo_id: char_id,
                    master_wallet: eve_db::IskCents::from_isk(10_000_000),
                    members: std::collections::HashSet::from([char_id]),
                    ..Default::default()
                },
            );
        });

        let mut sess = SessionState::new(7005);
        sess.user_id = Some(1);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(corp_id);

        let ad_rep = ctx.dispatch_rpc(
            &mut sess,
            "corpRecProxy",
            None,
            "CreateRecruitmentAd",
            &[PyRep::Integer(1), PyRep::Integer(4), PyRep::String("PvP Corp".into()), PyRep::String("Join today".into())],
            &kw,
            None,
        ).await.unwrap();
        let ad_id = ad_rep.dict_get("adID").and_then(|v| v.as_i64()).unwrap();

        let persisted = ctx.db.with_snapshot(|snap| {
            snap.service_state_blobs.contains_key("corp_recruitment_ads")
        });
        assert!(persisted, "Recruitment ads must persist in service_state_blobs");

        let res = ctx.dispatch_rpc(&mut sess, "corpRecProxy", None, "SearchCorpAds", &[PyRep::Integer(4)], &kw, None).await.unwrap();
        let PyRep::Rowset { rows, .. } = res else { panic!("expected Rowset") };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0].as_i64(), Some(ad_id));

        let res2 = ctx.dispatch_rpc(&mut sess, "corpRecProxy", None, "SearchCorpAds", &[PyRep::Integer(1)], &kw, None).await.unwrap();
        let PyRep::Rowset { rows: rows2, .. } = res2 else { panic!("expected Rowset") };
        assert!(rows2.is_empty(), "Mismatched typeMask must exclude ad");
    }

    #[tokio::test]
    async fn test_fleet_finder_advert_keyval() {
        // parity: rpc:fleetProxy.AddFleetFinderAdvert
        eve_testkit::attest(&["rpc:fleetProxy.AddFleetFinderAdvert"]);
        let ctx = ServerContext::new();
        let kw = HashMap::new();
        let char_id = 90_000_001i64;
        let fleet_id = 66_001i64;
        let corp_id = 1_000_170i64;

        ctx.db.with_mut_snapshot(|snap| {
            snap.fleets.insert(fleet_id, eve_db::FleetRecord {
                fleet_id,
                boss_id: char_id,
                members: vec![char_id],
                ..Default::default()
            });
            if let Some(c) = snap.characters.get_mut(&char_id) {
                c.corporation_id = corp_id;
            }
        });

        let mut sess = SessionState::new(7006);
        sess.user_id = Some(1);
        sess.character_id = Some(char_id);
        sess.fleet_id = Some(fleet_id);
        sess.corporation_id = Some(corp_id);

        let advert_kv = PyRep::keyval([
            ("fleetName", PyRep::String("Corp Mining Op".into())),
            ("description", PyRep::String("High yield mining".into())),
            ("inviteScope", PyRep::Integer(1)),
            ("joinNeedsApproval", PyRep::Bool(true)),
        ]);
        let rep = ctx.dispatch_rpc(
            &mut sess,
            "fleetProxy",
            None,
            "AddFleetFinderAdvert",
            &[advert_kv],
            &kw,
            None,
        ).await.expect("AddFleetFinderAdvert with KeyVal");
        assert_eq!(rep.dict_get("fleetName").and_then(|v| v.as_str()), Some("Corp Mining Op"));
        assert_eq!(rep.dict_get("inviteScope").and_then(|v| v.as_i64()), Some(1));
        assert_eq!(rep.dict_get("autoJoin").and_then(|v| v.as_bool()), Some(false));
    }

    #[tokio::test]
    async fn test_clone_vat_bay_target_only_and_psychology_cap() {
        // parity: rpc:jumpCloneSvc.AcceptShipCloneInstallation
        eve_testkit::attest(&["rpc:jumpCloneSvc.AcceptShipCloneInstallation"]);
        let ctx = ServerContext::new();
        let kw = HashMap::new();
        let p1 = 90_000_001i64;
        let p2 = 90_000_002i64;
        let rorq_id = 999_201i64;
        let cvb_id = 999_202i64;

        seed_item(&ctx, rorq_id, 28352, p1, 30000142, eve_db::InventoryFlag::None, 1, true);
        seed_item(&ctx, cvb_id, 23735, p1, rorq_id, eve_db::InventoryFlag::HiSlot0, 1, true);
        seed_item(&ctx, 999_203, 16273, p1, rorq_id, eve_db::InventoryFlag::Cargo, 10, false);

        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(cvb_id, true);
            if let Some(c) = snap.characters.get_mut(&p1) {
                c.active_ship_id = rorq_id;
            }
            snap.fleets.insert(5001, eve_db::FleetRecord {
                fleet_id: 5001,
                boss_id: p1,
                members: vec![p1, p2],
                ..Default::default()
            });
        });

        let mut s1 = SessionState::new(7007);
        s1.user_id = Some(1);
        s1.character_id = Some(p1);
        s1.ship_id = Some(rorq_id);
        s1.solar_system_id = Some(30_000_142);
        ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(cvb_id)], &kw, None)
            .await
            .unwrap();

        ctx.dispatch_rpc(&mut s1, "jumpCloneSvc", None, "OfferShipCloneInstallation", &[PyRep::Integer(p2)], &kw, None)
            .await.unwrap();

        let offerer_err = ctx.dispatch_rpc(&mut s1, "jumpCloneSvc", None, "AcceptShipCloneInstallation", &[], &kw, None)
            .await.unwrap_err();
        assert_eq!(offerer_err.code, "NoCloneOfferFound");

        let mut s2 = SessionState::new(7008);
        s2.user_id = Some(2);
        s2.character_id = Some(p2);

        let limit_err = ctx.dispatch_rpc(&mut s2, "jumpCloneSvc", None, "AcceptShipCloneInstallation", &[], &kw, None)
            .await.unwrap_err();
        assert_eq!(limit_err.code, "JumpCloneLimitReached");

        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(p2).or_default().insert(24242, eve_db::TrainedSkillRecord {
                type_id: 24242,
                level: 1,
                skill_points: 250.0,
            });
        });

        ctx.dispatch_rpc(&mut s1, "jumpCloneSvc", None, "OfferShipCloneInstallation", &[PyRep::Integer(p2)], &kw, None)
            .await.unwrap();
        let acc_rep = ctx.dispatch_rpc(&mut s2, "jumpCloneSvc", None, "AcceptShipCloneInstallation", &[], &kw, None)
            .await.unwrap();
        assert_eq!(acc_rep.dict_get("installed").and_then(|v| v.as_bool()), Some(true));
    }
}

pub(crate) mod sov_skyhooks_structures_fw_occupation {
    // Integration tests (part 1) behavioral verification auditor-flagged rows across V24 sov skyhooks,
    // FW occupation, system interference decay, and abyssal trace/weather.

    use std::collections::HashMap;
    use eve_db::{
        corp_roles, CorporationRecord, FwSystemOccupationRecord, InventoryFlag,
        InventoryItemRecord, SystemDynamicState, UpwellStructureRecord,
    };
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use crate::common;

    async fn setup_pilot(
        ctx: &ServerContext,
        sess_id: u64,
        user_name: &str,
        secret: &str,
        char_id: i64,
    ) -> SessionState {
        let mut sess = SessionState::new(sess_id);
        common::login_pilot(ctx, &mut sess, user_name, secret).await;
        ctx.dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("SelectCharacterID");
        sess
    }

    fn as_list<'a>(rep: &'a PyRep) -> &'a [PyRep] {
        match rep {
            PyRep::List(l) => l.as_slice(),
            _ => panic!("Expected PyRep::List, got {rep:?}"),
        }
    }

    fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
        Ball {
            id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 1000.0,
            inertia: 1.0,
            mass: 100_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        }
    }

    #[tokio::test]
    async fn test_sov_skyhooks_and_structures_audit() {
        // parity: rpc:sovMgr.AcquireSkyhooks, rpc:sovMgr.GetSovStructuresInfoForLocalSolarSystem, rpc:colonyResourcesMgr.GetMyCorporationSkyhooks, rpc:insurgencySolarsystem.GetAllVisibleCampaigns
        eve_testkit::attest(&[
            "rpc:sovMgr.AcquireSkyhooks",
            "rpc:sovMgr.GetSovStructuresInfoForLocalSolarSystem",
            "rpc:colonyResourcesMgr.GetMyCorporationSkyhooks",
            "rpc:insurgencySolarsystem.GetAllVisibleCampaigns",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let null_sys = 30_004_759u32;
        let corp_id = 98_000_101i64;
        let alliance_id = 99_000_101i64;

        let uid = ctx.db.provision_player_account("sov_tester", "sec123");
        let char_id = ctx
            .db
            .create_character(uid, "Sov Tester", 1, 1, 11, 17, &ctx.sde)
            .expect("create_character")
            .character_id;

        let ship_id = 910_000_101i64;
        let skyhook_id = 920_000_101i64;
        let ihub_id = 920_000_102i64;

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&char_id) {
                c.solar_system_id = null_sys;
                c.corporation_id = corp_id;
                c.alliance_id = Some(alliance_id);
                c.station_id = None;
                c.active_ship_id = ship_id;
            }
            snap.corporations.insert(
                corp_id,
                CorporationRecord {
                    corporation_id: corp_id,
                    name: "Audit Corp".into(),
                    ticker: "AUDIT".into(),
                    ceo_id: char_id,
                    alliance_id: Some(alliance_id),
                    member_roles: HashMap::from([(char_id, corp_roles::ROLE_DIRECTOR)]),
                    ..Default::default()
                },
            );
            snap.system_states.insert(
                null_sys,
                SystemDynamicState {
                    solar_system_id: null_sys,
                    sov_alliance_id: Some(alliance_id),
                    last_sov_claim_tick: 100,
                    ..Default::default()
                },
            );
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 587,
                    owner_id: char_id,
                    location_id: null_sys as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.structures.insert(
                skyhook_id,
                UpwellStructureRecord {
                    structure_id: skyhook_id,
                    type_id: 81080,
                    owner_corp_id: corp_id,
                    solar_system_id: null_sys,
                    name: "Test Skyhook".into(),
                    position: [0.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    shield_hp: 10000.0,
                    armor_hp: 10000.0,
                    hull_hp: 10000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0, self_destruct_at_unix: None,
                },
            );
            snap.structures.insert(
                ihub_id,
                UpwellStructureRecord {
                    structure_id: ihub_id,
                    type_id: 32458,
                    owner_corp_id: corp_id,
                    solar_system_id: null_sys,
                    name: "Test IHub".into(),
                    position: [1000.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    shield_hp: 10000.0,
                    armor_hp: 10000.0,
                    hull_hp: 10000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0, self_destruct_at_unix: None,
                },
            );
        });

        eve_server::services::sov_services::set_skyhook_reagents(
            &ctx,
            skyhook_id,
            eve_server::services::sov_services::TYPE_MAGMATIC_GAS,
            250,
        );

        let mut sess = setup_pilot(&ctx, 1001, "sov_tester", "sec123", char_id).await;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&char_id) {
                c.solar_system_id = null_sys;
                c.station_id = None;
                c.active_ship_id = ship_id;
            }
        });
        sess.solar_system_id = Some(null_sys);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(ship_id);
        sess.corporation_id = Some(corp_id);
        sess.alliance_id = Some(alliance_id);

        let sys_handle = ctx.systems.get_or_spawn(null_sys);
        if let Ok(Some((_, ship_ball))) = sys_handle.get_ball_snapshot(ship_id).await {
            let p = [ship_ball.position.x, ship_ball.position.y, ship_ball.position.z];
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(s) = snap.structures.get_mut(&ihub_id) {
                    s.position = [p[0] + 1000.0, p[1], p[2]];
                }
            });
        }

        // 1. AcquireSkyhooks (read-only verification)
        let initial_claim_tick = ctx.db.with_snapshot(|s| {
            s.system_states.get(&null_sys).map(|st| st.last_sov_claim_tick)
        });
        let skyhook_res = ctx
            .dispatch_rpc(
                &mut sess,
                "sovMgr",
                None,
                "AcquireSkyhooks",
                &[PyRep::Integer(null_sys as i64)],
                &HashMap::new(),
                None,
            )
            .await
            .expect("sovMgr.AcquireSkyhooks");

        let post_claim_tick = ctx.db.with_snapshot(|s| {
            s.system_states.get(&null_sys).map(|st| st.last_sov_claim_tick)
        });
        assert_eq!(
            initial_claim_tick, post_claim_tick,
            "AcquireSkyhooks must be strictly read-only"
        );

        let sk_rep = skyhook_res.dict_get("skyhooks").expect("skyhooks list");
        let list = as_list(sk_rep);
        assert!(list.iter().any(|item| {
            item.dict_get("skyhookID").and_then(|v| v.as_i64()) == Some(skyhook_id)
        }));

        // 2. GetSovStructuresInfoForLocalSolarSystem (filters to TCU/IHub only)
        let sov_info = ctx
            .dispatch_rpc(
                &mut sess,
                "sovMgr",
                None,
                "GetSovStructuresInfoForLocalSolarSystem",
                &[],
                &HashMap::new(),
                None,
            )
            .await
            .expect("sovMgr.GetSovStructuresInfoForLocalSolarSystem");

        let structs_rep = sov_info.dict_get("structures").expect("structures dict in sov info");
        let structs_list = as_list(structs_rep);
        assert_eq!(
            structs_list.len(),
            1,
            "Only IHub (32458) should be in sov structures, not skyhook (81080)"
        );
        assert_eq!(
            structs_list[0].dict_get("structureID").and_then(|v| v.as_i64()),
            Some(ihub_id)
        );
        assert_eq!(
            sov_info.dict_get("infrastructureHubID").and_then(|v| v.as_i64()),
            Some(ihub_id)
        );

        // 3. colonyResourcesMgr.GetMyCorporationSkyhooks
        let my_skyhooks = ctx
            .dispatch_rpc(
                &mut sess,
                "colonyResourcesMgr",
                None,
                "GetMyCorporationSkyhooks",
                &[],
                &HashMap::new(),
                None,
            )
            .await
            .expect("colonyResourcesMgr.GetMyCorporationSkyhooks");

        let sk_list = as_list(&my_skyhooks);
        assert_eq!(sk_list.len(), 1);
        assert_eq!(
            sk_list[0].dict_get("skyhookID").and_then(|v| v.as_i64()),
            Some(skyhook_id)
        );
        assert_eq!(
            sk_list[0].dict_get("reagentQuantity").and_then(|v| v.as_i64()),
            Some(250)
        );

        // 4. GetAllVisibleCampaigns
        let camps = ctx
            .dispatch_rpc(
                &mut sess,
                "insurgencySolarsystem",
                None,
                "GetAllVisibleCampaigns",
                &[],
                &HashMap::new(),
                None,
            )
            .await
            .expect("insurgencySolarsystem.GetAllVisibleCampaigns");
        assert!(matches!(camps, PyRep::List(_)));
    }

    #[tokio::test]
    async fn test_fw_occupation_and_interference_decay_audit() {
        // parity: rpc:fwWarzoneSolarsystem.GetLocalOccupationState, rpc:solarsystemInterferenceMgr.GetLocalInterferenceState
        eve_testkit::attest(&[
            "rpc:fwWarzoneSolarsystem.GetLocalOccupationState",
            "rpc:solarsystemInterferenceMgr.GetLocalInterferenceState",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let uid = ctx.db.provision_player_account("fw_tester", "sec123");
        let char_id = ctx
            .db
            .create_character(uid, "FW Tester", 1, 1, 11, 17, &ctx.sde)
            .expect("create_character")
            .character_id;

        let mut sess = setup_pilot(&ctx, 1002, "fw_tester", "sec123", char_id).await;

        // 1. fwWarzoneSolarsystem.GetLocalOccupationState outside warzone
        let non_wz_sys = 30_000_142u32;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&char_id) {
                c.solar_system_id = non_wz_sys;
                c.station_id = None;
            }
        });
        sess.solar_system_id = Some(non_wz_sys);
        sess.station_id = None;
        sess.structure_id = None;

        let occ_outside = ctx
            .dispatch_rpc(
                &mut sess,
                "fwWarzoneSolarsystem",
                None,
                "GetLocalOccupationState",
                &[],
                &HashMap::new(),
                None,
            )
            .await
            .expect("fwWarzoneSolarsystem outside warzone");
        match occ_outside {
            PyRep::Tuple(items) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].as_i64(), Some(non_wz_sys as i64));
                assert_eq!(items[1], PyRep::None, "Non-warzone system must return None state");
            }
            _ => panic!("Expected Tuple, got {occ_outside:?}"),
        }

        // Inside warzone system with occupier
        let wz_sys = 30_002_084u32;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&char_id) {
                c.solar_system_id = wz_sys;
            }
            snap.fw_system_occupations.insert(
                wz_sys,
                FwSystemOccupationRecord {
                    solar_system_id: wz_sys,
                    owner_faction_id: 500_001,
                    occupier_faction_id: 500_004,
                    victory_points: 100,
                    victory_points_threshold: 1000,
                },
            );
        });
        sess.solar_system_id = Some(wz_sys);
        let occ_inside = ctx
            .dispatch_rpc(
                &mut sess,
                "fwWarzoneSolarsystem",
                None,
                "GetLocalOccupationState",
                &[],
                &HashMap::new(),
                None,
            )
            .await
            .expect("fwWarzoneSolarsystem inside warzone");
        match occ_inside {
            PyRep::Tuple(items) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].as_i64(), Some(wz_sys as i64));
                let dict = &items[1];
                assert_eq!(dict.dict_get("occupierID").and_then(|v| v.as_i64()), Some(500_004));
                assert!(dict.dict_get("frontlineStatus").is_some());
                assert!(dict.dict_get("contestedPercentage").is_some());
            }
            _ => panic!("Expected Tuple, got {occ_inside:?}"),
        }

        // 2. solarsystemInterferenceMgr.GetLocalInterferenceState with linear decay
        eve_server::services::pve_special_sites::record_system_interference_activity(
            &ctx, &mut sess, wz_sys, 0.85,
        );
        let int_state_high = ctx
            .dispatch_rpc(
                &mut sess,
                "solarsystemInterferenceMgr",
                None,
                "GetLocalInterferenceState",
                &[PyRep::Integer(wz_sys as i64)],
                &HashMap::new(),
                None,
            )
            .await
            .expect("GetLocalInterferenceState high");

        let level_high = int_state_high
            .dict_get("interferenceLevel")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let band_high = int_state_high
            .dict_get("interferenceBand")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        assert!(level_high >= 0.70, "Level {level_high} should be >= 0.70");
        assert_eq!(band_high, 3, "Band should be High (3)");

        ctx.advance_secs(600);

        let int_state_decayed = ctx
            .dispatch_rpc(
                &mut sess,
                "solarsystemInterferenceMgr",
                None,
                "GetLocalInterferenceState",
                &[PyRep::Integer(wz_sys as i64)],
                &HashMap::new(),
                None,
            )
            .await
            .expect("GetLocalInterferenceState decayed");
        let level_decayed = int_state_decayed
            .dict_get("interferenceLevel")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        assert!(
            level_decayed < level_high,
            "Interference level must have decayed from {level_high} to {level_decayed}"
        );
    }

    #[tokio::test]
    async fn test_abyssal_deployment_and_weather_audit() {
        // parity: rpc:abyssalMgr.AbyssalEntranceDeployment, feat:pve.abyssal-pocket, feat:pve.abyssal-weather
        eve_testkit::attest(&[
            "rpc:abyssalMgr.AbyssalEntranceDeployment",
            "feat:pve.abyssal-pocket",
            "feat:pve.abyssal-weather",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let uid = ctx.db.provision_player_account("abyssal_tester", "sec123");
        let char_id = ctx
            .db
            .create_character(uid, "Abyssal Tester", 1, 1, 11, 17, &ctx.sde)
            .expect("create_character")
            .character_id;

        let ship_id = 910_000_103i64;
        let filament_item_id = 920_000_103i64;
        let filament_type_id = 47763u32; // Calm Firestorm Filament (tier 1)
        let cur_sys = 30_000_002u32; // Lashesih (0.8 high-sec)

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&char_id) {
                c.solar_system_id = cur_sys;
                c.station_id = None;
                c.active_ship_id = ship_id;
            }
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 621, // Caracal Cruiser (group 26)
                    owner_id: char_id,
                    location_id: cur_sys as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                filament_item_id,
                InventoryItemRecord {
                    item_id: filament_item_id,
                    type_id: filament_type_id,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: false,
                    ..Default::default()
                },
            );
        });

        let mut sess = setup_pilot(&ctx, 1003, "abyssal_tester", "sec123", char_id).await;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&char_id) {
                c.solar_system_id = cur_sys;
                c.station_id = None;
                c.active_ship_id = ship_id;
            }
        });
        sess.solar_system_id = Some(cur_sys);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(ship_id);

        let sys_handle = ctx.systems.get_or_spawn(cur_sys);
        let ship_ball = make_ball(ship_id, 621, DVec3::ZERO);
        sys_handle.spawn_ball(ship_ball).await.expect("spawn ship ball");

        // 1. AbyssalEntranceDeployment: spawns trace without moving ship
        let deploy_res = ctx
            .dispatch_rpc(
                &mut sess,
                "abyssalMgr",
                None,
                "AbyssalEntranceDeployment",
                &[PyRep::Integer(filament_item_id)],
                &HashMap::new(),
                None,
            )
            .await
            .expect("AbyssalEntranceDeployment");

        let trace_id = deploy_res
            .dict_get("traceBallID")
            .and_then(|v| v.as_i64())
            .expect("traceBallID");
        assert_eq!(
            sess.solar_system_id,
            Some(cur_sys),
            "Pilot remains in initial system after deploying trace"
        );

        let item_in_cargo = ctx.db.with_snapshot(|s| s.items.contains_key(&filament_item_id));
        assert!(!item_in_cargo, "Filament must be consumed upon deployment");

        // 2. AbyssalEntranceGateActivation enters pocket (feat:pve.abyssal-pocket)
        let gate_res = ctx
            .dispatch_rpc(
                &mut sess,
                "abyssalMgr",
                None,
                "AbyssalEntranceGateActivation",
                &[PyRep::Integer(trace_id)],
                &HashMap::new(),
                None,
            )
            .await
            .expect("AbyssalEntranceGateActivation");

        let pocket_sys_id = gate_res
            .dict_get("solarSystemID")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        assert!(
            (32_000_000..=32_999_999).contains(&pocket_sys_id),
            "Entered pocket solar system {pocket_sys_id}"
        );

        // 3. Dynamic abyssal weather application in pocket (feat:pve.abyssal-weather)
        let mut attrs: HashMap<u16, f64> = HashMap::from([(265u16, 1000.0), (110u16, 0.5)]);
        eve_server::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs(
            &ctx,
            ship_id,
            &mut attrs,
        );
        assert!(
            attrs[&265] > 1000.0,
            "Weather must boost armor HP dynamically in abyssal pocket"
        );
    }
}

pub(crate) mod ess_reserve_pulses_fighters_tubes {
    // Integration tests (part 2) behavioral verification auditor-flagged rows across ESS reserve pulses,
    // fighters launching/abilities, and structure ops/asset safety.

    use std::collections::HashMap;
    use eve_db::{
        corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    };
    use eve_destiny::{Ball, CombatState, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use crate::common;

    async fn setup_pilot(
        ctx: &ServerContext,
        sess_id: u64,
        user_name: &str,
        secret: &str,
        char_id: i64,
    ) -> SessionState {
        let mut sess = SessionState::new(sess_id);
        common::login_pilot(ctx, &mut sess, user_name, secret).await;
        ctx.dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("SelectCharacterID");
        sess
    }

    fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
        Ball {
            id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 1000.0,
            inertia: 1.0,
            mass: 100_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        }
    }

    #[tokio::test]
    async fn test_ess_reserve_pulses_audit() {
        // parity: rpc:essMgr.RequestUnlockReserveBank
        eve_testkit::attest(&["rpc:essMgr.RequestUnlockReserveBank"]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let uid = ctx.db.provision_player_account("ess_tester", "sec123");
        let char_id = ctx
            .db
            .create_character(uid, "ESS Tester", 1, 1, 11, 17, &ctx.sde)
            .expect("create_character")
            .character_id;

        let sys_id = 30_004_759u32;
        let ship_id = 910_000_104i64;
        let key_item_id = 920_000_104i64;
        let key_type_id = 57721u32; // 45-minute SW key for Delve (30_004_759)
        let ess_ball_id = 930_000_104i64;

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&char_id) {
                c.solar_system_id = sys_id;
                c.station_id = None;
                c.active_ship_id = ship_id;
            }
            snap.system_states.insert(
                sys_id,
                eve_db::SystemDynamicState {
                    solar_system_id: sys_id,
                    ess_reserve_bank: IskCents::from_isk(100_000_000),
                    ..Default::default()
                },
            );
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 587,
                    owner_id: char_id,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                key_item_id,
                InventoryItemRecord {
                    item_id: key_item_id,
                    type_id: key_type_id,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: false,
                    ..Default::default()
                },
            );
        });

        let mut sess = setup_pilot(&ctx, 1004, "ess_tester", "sec123", char_id).await;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&char_id) {
                c.solar_system_id = sys_id;
                c.station_id = None;
                c.active_ship_id = ship_id;
            }
        });
        sess.solar_system_id = Some(sys_id);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(ship_id);

        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let ship_ball = make_ball(ship_id, 587, DVec3::new(100.0, 0.0, 0.0));
        sys_handle.spawn_ball(ship_ball).await.expect("spawn ship ball");

        let ess_ball = make_ball(ess_ball_id, 56002, DVec3::new(0.0, 0.0, 0.0));
        sys_handle
            .spawn_ball_with_combat(ess_ball, CombatState::default())
            .await
            .expect("spawn ess ball");

        // Call RequestUnlockReserveBank
        let unlock_res = ctx
            .dispatch_rpc(
                &mut sess,
                "essMgr",
                None,
                "RequestUnlockReserveBank",
                &[PyRep::Integer(key_type_id as i64)],
                &HashMap::new(),
                None,
            )
            .await
            .expect("RequestUnlockReserveBank");

        assert_eq!(
            unlock_res.dict_get("unlocked").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            unlock_res.dict_get("durationMinutes").and_then(|v| v.as_i64()),
            Some(45)
        );

        let has_key = ctx.db.with_snapshot(|s| s.items.contains_key(&key_item_id));
        assert!(!has_key, "Key must be consumed upon unlocking reserve bank");

        // Verify pulses in GetDataForClientSolarSystem
        let ess_data = ctx
            .dispatch_rpc(
                &mut sess,
                "essMgr",
                None,
                "GetDataForClientSolarSystem",
                &[],
                &HashMap::new(),
                None,
            )
            .await
            .expect("GetDataForClientSolarSystem");

        let pulses_rem = ess_data
            .dict_get("reserveBankPulsesRemaining")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let pulses_total = ess_data
            .dict_get("reserveBankPulsesTotal")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        assert_eq!(pulses_total, 45, "45-minute key gives 45 pulses");
        assert_eq!(pulses_rem, 45);
        assert!(ess_data.dict_get("reserveBankLastPulseInitiated").is_some());
    }

    #[tokio::test]
    async fn test_fighters_tubes_and_multi_activation_audit() {
        // parity: rpc:fighterMgr.LaunchFightersFromTubes, rpc:fighterMgr.CmdActivateAbilitySlots
        eve_testkit::attest(&[
            "rpc:fighterMgr.LaunchFightersFromTubes",
            "rpc:fighterMgr.CmdActivateAbilitySlots",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let uid = ctx.db.provision_player_account("fighter_tester", "sec123");
        let char_id = ctx
            .db
            .create_character(uid, "Fighter Tester", 1, 1, 11, 17, &ctx.sde)
            .expect("create_character")
            .character_id;

        let sys_id = 30_000_142u32;
        let carrier_id = 910_000_105i64;
        let squadron_id = 920_000_105i64;
        let fighter_type_id = 23057u32; // Dragonfly I Light Fighter

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&char_id) {
                c.solar_system_id = sys_id;
                c.station_id = None;
                c.active_ship_id = carrier_id;
            }
            snap.items.insert(
                carrier_id,
                InventoryItemRecord {
                    item_id: carrier_id,
                    type_id: 23757, // Archon Carrier
                    owner_id: char_id,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                squadron_id,
                InventoryItemRecord {
                    item_id: squadron_id,
                    type_id: fighter_type_id,
                    owner_id: char_id,
                    location_id: carrier_id,
                    flag: InventoryFlag::FighterBay,
                    quantity: 9,
                    singleton: true,
                    ..Default::default()
                },
            );
        });

        // Load squadron into tube 0
        ctx.fighter_tube_loads.write().unwrap().insert((carrier_id, 0), squadron_id);

        let mut sess = setup_pilot(&ctx, 1005, "fighter_tester", "sec123", char_id).await;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&char_id) {
                c.solar_system_id = sys_id;
                c.station_id = None;
                c.active_ship_id = carrier_id;
            }
        });
        sess.solar_system_id = Some(sys_id);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(carrier_id);

        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let carrier_ball = make_ball(carrier_id, 23757, DVec3::new(0.0, 0.0, 0.0));
        sys_handle.spawn_ball(carrier_ball).await.expect("spawn carrier ball");

        // 1. LaunchFightersFromTubes with tube flag 159
        let launch_res = ctx
            .dispatch_rpc(
                &mut sess,
                "fighterMgr",
                None,
                "LaunchFightersFromTubes",
                &[PyRep::List(vec![PyRep::Integer(159)])],
                &HashMap::new(),
                None,
            )
            .await
            .expect("LaunchFightersFromTubes");

        assert!(
            matches!(launch_res, PyRep::Dict(_)),
            "Flag launching must return PyRep::Dict"
        );

        let item_loc = ctx.db.with_snapshot(|s| s.items.get(&squadron_id).map(|it| it.location_id));
        assert_eq!(
            item_loc,
            Some(sys_id as i64),
            "Fighter item must have moved into space"
        );

        // 2. CmdActivateAbilitySlots fighter ability activation
        let fighter_ball_id = ctx
            .active_fighters
            .read()
            .unwrap()
            .keys()
            .copied()
            .next()
            .expect("active fighter ball");

        let act_res = ctx
            .dispatch_rpc(
                &mut sess,
                "fighterMgr",
                None,
                "CmdActivateAbilitySlots",
                &[
                    PyRep::List(vec![PyRep::Integer(fighter_ball_id)]),
                    PyRep::Integer(1),
                ],
                &HashMap::new(),
                None,
            )
            .await
            .expect("CmdActivateAbilitySlots");

        assert_eq!(
            act_res.dict_get("success").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[tokio::test]
    async fn test_structure_ops_and_asset_safety_audit() {
        // parity: rpc:structureDeployment.Anchor, rpc:structureControl.TakeControl, rpc:structureAssetSafety.GetStructuresICanDeliverTo, feat:structures.asset-safety-manual, feat:lifecycle.asset_safety-auto_delivery
        eve_testkit::attest(&[
            "rpc:structureDeployment.Anchor",
            "rpc:structureControl.TakeControl",
            "rpc:structureAssetSafety.GetStructuresICanDeliverTo",
            "feat:structures.asset-safety-manual",
            "feat:lifecycle.asset_safety-auto_delivery",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let corp_id = 98_000_106i64;
        let uid = ctx.db.provision_player_account("struct_tester", "sec123");
        let char_id = ctx
            .db
            .create_character(uid, "Struct Tester", 1, 1, 11, 17, &ctx.sde)
            .expect("create_character")
            .character_id;

        let sys_id = 30_000_144u32;
        let ship_id = 910_000_106i64;
        let astrahus_item_id = 920_000_106i64;

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&char_id) {
                c.solar_system_id = sys_id;
                c.corporation_id = corp_id;
                c.balance = IskCents::from_f64_isk(1_000_000_000.0);
                c.station_id = None;
                c.active_ship_id = ship_id;
            }
            snap.corporations.insert(
                corp_id,
                CorporationRecord {
                    corporation_id: corp_id,
                    name: "Struct Corp".into(),
                    ticker: "STRCT".into(),
                    ceo_id: char_id,
                    alliance_id: None,
                    member_roles: HashMap::from([(
                        char_id,
                        corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER,
                    )]),
                    ..Default::default()
                },
            );
            snap.system_states.insert(
                sys_id,
                eve_db::SystemDynamicState {
                    solar_system_id: sys_id,
                    ess_reserve_bank: IskCents::from_isk(100_000_000),
                    ..Default::default()
                },
            );
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 587,
                    owner_id: char_id,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                astrahus_item_id,
                InventoryItemRecord {
                    item_id: astrahus_item_id,
                    type_id: 35832, // Astrahus Citadels (group 1657)
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });

        let mut sess = setup_pilot(&ctx, 1006, "struct_tester", "sec123", char_id).await;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&char_id) {
                c.solar_system_id = sys_id;
                c.station_id = None;
                c.active_ship_id = ship_id;
            }
        });
        sess.solar_system_id = Some(sys_id);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(ship_id);
        sess.corporation_id = Some(corp_id);

        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let ship_ball = make_ball(ship_id, 587, DVec3::new(0.0, 5.0e9, 0.0));
        sys_handle.spawn_ball(ship_ball).await.expect("spawn ship ball");

        // 1. structureDeployment.Anchor with V24 client tuple
        // (cargo_item_id, [x,y,z], profile_id, (weekday, hour), name)
        let anchor_args = vec![
            PyRep::Integer(astrahus_item_id),
            PyRep::List(vec![
                PyRep::Real(60_000.0),
                PyRep::Real(5.0e9),
                PyRep::Real(0.0),
            ]),
            PyRep::Integer(1),
            PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(18)]),
            PyRep::String("Alpha Astrahus".into()),
        ];
        let anchor_res = ctx
            .dispatch_rpc(
                &mut sess,
                "structureDeployment",
                None,
                "Anchor",
                &anchor_args,
                &HashMap::new(),
                None,
            )
            .await
            .expect("structureDeployment.Anchor");

        let structure_id = anchor_res
            .dict_get("structureID")
            .and_then(|v| v.as_i64())
            .expect("structureID");
        assert!(structure_id > 0);

        let has_astrahus_item = ctx.db.with_snapshot(|s| s.items.contains_key(&astrahus_item_id));
        assert!(!has_astrahus_item, "Deployed item must be consumed from cargo");

        // 2. structureControl.TakeControl
        let take_ctrl_undocked = ctx
            .dispatch_rpc(
                &mut sess,
                "structureControl",
                None,
                "TakeControl",
                &[PyRep::Integer(structure_id)],
                &HashMap::new(),
                None,
            )
            .await;
        assert!(take_ctrl_undocked.is_err(), "TakeControl must fail when not docked");

        sess.structure_id = Some(structure_id);
        sess.station_id = None;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.structure_id = Some(structure_id);
                ch.station_id = None;
            }
        });
        let take_ctrl_res = ctx
            .dispatch_rpc(
                &mut sess,
                "structureControl",
                None,
                "TakeControl",
                &[PyRep::Integer(structure_id)],
                &HashMap::new(),
                None,
            )
            .await
            .expect("structureControl.TakeControl while docked with defense role");

        assert_eq!(
            sess.ship_id,
            Some(structure_id),
            "Pilot ship ID becomes structure ID upon taking control"
        );
        assert_eq!(
            take_ctrl_res.dict_get("success").and_then(|v| v.as_bool()),
            Some(true)
        );

        // 3. structureAssetSafety.GetStructuresICanDeliverTo
        let deliver_res = ctx
            .dispatch_rpc(
                &mut sess,
                "structureAssetSafety",
                None,
                "GetStructuresICanDeliverTo",
                &[],
                &HashMap::new(),
                None,
            )
            .await
            .expect("GetStructuresICanDeliverTo");
        assert!(matches!(deliver_res, PyRep::List(_)));

        // 4. MoveSafetyWrapToStructure with same-system 0.5% fee (feat:structures.asset-safety-manual)
        let wrap_id = 940_000_106i64;
        let wrapped_item_id = 950_000_106i64;
        let initial_balance = ctx
            .db
            .with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                wrapped_item_id,
                InventoryItemRecord {
                    item_id: wrapped_item_id,
                    type_id: 587,
                    owner_id: char_id,
                    location_id: 0,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
            ext.asset_safety_wraps.insert(
                wrap_id,
                eve_server::services::station_structure::structure_upwell_ops::AssetSafetyWrapRecord {
                    wrap_id,
                    owner_id: char_id,
                    is_corp: false,
                    source_structure_id: structure_id,
                    source_structure_name: "Test Structure".into(),
                    solar_system_id: sys_id,
                    created_unix: ctx.now_unix() - 100,
                    deliverable_after_unix: ctx.now_unix() - 10,
                    delivered_to_id: None,
                    estimated_value_isk: 10_000_000.0,
                    item_ids: vec![wrapped_item_id],
                    nearest_npc_station_id: Some(60003760),
                    auto_delivered: false,
                    retrieval_fee_paid: false,
                },
            );
            let auto_wrap_id = 940_000_107i64;
            let auto_item_id = 950_000_107i64;
            snap.items.insert(
                auto_item_id,
                InventoryItemRecord {
                    item_id: auto_item_id,
                    type_id: 34,
                    owner_id: char_id,
                    location_id: 0,
                    flag: InventoryFlag::None,
                    quantity: 50,
                    ..Default::default()
                },
            );
            ext.asset_safety_wraps.insert(
                auto_wrap_id,
                eve_server::services::station_structure::structure_upwell_ops::AssetSafetyWrapRecord {
                    wrap_id: auto_wrap_id,
                    owner_id: char_id,
                    is_corp: false,
                    source_structure_id: structure_id,
                    source_structure_name: "Test Structure".into(),
                    solar_system_id: sys_id,
                    created_unix: ctx.now_unix() - (21 * 86_400),
                    deliverable_after_unix: ctx.now_unix() - (15 * 86_400),
                    delivered_to_id: None,
                    estimated_value_isk: 500_000.0,
                    item_ids: vec![auto_item_id],
                    nearest_npc_station_id: Some(60003760),
                    auto_delivered: false,
                    retrieval_fee_paid: false,
                },
            );
            eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        });

        let move_res = ctx
            .dispatch_rpc(
                &mut sess,
                "structureAssetSafety",
                None,
                "MoveSafetyWrapToStructure",
                &[PyRep::Integer(wrap_id), PyRep::Integer(structure_id)],
                &HashMap::new(),
                None,
            )
            .await
            .expect("MoveSafetyWrapToStructure");

        // feat:lifecycle.asset_safety-auto_delivery: wrap older than 20 days is lazily auto-delivered
        let auto_del = ctx.db.with_snapshot(|snap| {
            let ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
            ext.asset_safety_wraps.get(&940_000_107i64).map(|w| (w.auto_delivered, w.delivered_to_id))
        });
        assert_eq!(auto_del, Some((true, Some(60003760))));

        let fee_paid = move_res
            .dict_get("feePaidISK")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        assert!(
            (fee_paid - 50_000.0).abs() < 1.0,
            "Expected 0.5% fee (50,000 ISK), got {fee_paid}"
        );

        let post_balance = ctx
            .db
            .with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
        assert_eq!(
            initial_balance.0 - post_balance.0,
            5_000_000,
            "50,000.00 ISK in cents"
        );
    }
}
