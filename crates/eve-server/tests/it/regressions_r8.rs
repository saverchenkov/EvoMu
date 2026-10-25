//! Integration regression tests covering Round 8 findings.

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_review_r8_regressions() {
    // parity: rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID, rpc:dogmaIM.LoadAmmo, rpc:dynamicItemService.CreateDynamicItem, rpc:fighterMgr.CmdActivateAbilitySlots, rpc:industryManager.CompleteJob, rpc:industryManager.InstallJob, rpc:jumpCloneSvc.GetPriceForClone, rpc:keeper.ActivateAccelerationGate, rpc:marketProxy.BuyMultipleItems, rpc:marketProxy.GetCharOrders, rpc:marketProxy.PlaceBuyOrder, rpc:marketProxy.PlaceMultiSellOrder, rpc:multiLoginBlocker.Login, rpc:planetMgr.UserUpdateNetwork, rpc:repackagingSvc.RepackageItems, rpc:ship.Board, rpc:shipCosmeticsMgr.ActivateSkinLicense, rpc:shipCosmeticsMgr.ApplySkinToShip, rpc:shipCosmeticsMgr.GiveSkin, rpc:standingMgr.GetStandingTransactions
    use eve_db::InventoryFlag;

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut s1 = SessionState::new(30);
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

    let mut s2 = SessionState::new(31);
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

    let muta_locked = ctx
        .dispatch_rpc(
            &mut s1,
            "dynamicItemService",
            None,
            "CreateDynamicItem",
            &[PyRep::Integer(47746), PyRep::Integer(100_000_003)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        muta_locked.unwrap_err().code,
        "TargetItemLockedOrUnreachable"
    );

    // Verify AcceptContract fails with ContractEscrowItemMissing and zero ISK transfer if the escrowed item is missing
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&100_000_003) {
            it.flag = InventoryFlag::Hangar;
        }
    });
    let s2_bal_before = ctx.db.get_character(90_000_002).unwrap().balance.0;
    let missing_acc = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "AcceptContract",
            &[PyRep::Integer(cid_ab)],
            &kw,
            None,
        )
        .await;
    assert_eq!(missing_acc.unwrap_err().code, "ContractEscrowItemMissing");
    assert_eq!(
        ctx.db.get_character(90_000_002).unwrap().balance.0,
        s2_bal_before
    );

    // Restore lock and delete contract via DeleteContract; verify item 100_000_003 is back in Hangar at 60_003_760
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&100_000_003) {
            it.flag = InventoryFlag::Locked;
        }
    });
    ctx.dispatch_rpc(
        &mut s1,
        "contractProxy",
        None,
        "DeleteContract",
        &[PyRep::Integer(cid_ab)],
        &kw,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        ctx.db.get_item(100_000_003).unwrap().flag,
        InventoryFlag::Hangar
    );

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
    let comp_unlocked_err = ctx
        .dispatch_rpc(
            &mut s1,
            "industryManager",
            None,
            "CompleteJob",
            &[PyRep::Integer(jid)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        comp_unlocked_err.unwrap_err().code,
        "BlueprintMissingOrUnlocked"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(bp) = snap.items.get_mut(&100_000_007) {
            bp.flag = InventoryFlag::Locked;
        }
    });
    ctx.dispatch_rpc(
        &mut s1,
        "industryManager",
        None,
        "CompleteJob",
        &[PyRep::Integer(jid)],
        &kw,
        None,
    )
    .await
    .unwrap();

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
    assert_eq!(
        ctx.db.get_character(90_000_001).unwrap().balance.0,
        s1_bal_pre - 500_000
    );
    ctx.advance_virtual_clock(86_405);
    let acc_exp = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "AcceptContract",
            &[PyRep::Integer(cid_exp)],
            &kw,
            None,
        )
        .await;
    assert_eq!(acc_exp.unwrap_err().code, "ContractExpired");
    // Escrowed 5,000 ISK reward was durably refunded to issuer s1
    assert_eq!(
        ctx.db.get_character(90_000_001).unwrap().balance.0,
        s1_bal_pre
    );

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
    let acc_in = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "AcceptContract",
            &[PyRep::Integer(cid_in)],
            &kw,
            None,
        )
        .await
        .unwrap();
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
    let comp_exp = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "CompleteContract",
            &[PyRep::Integer(cid_in)],
            &kw,
            None,
        )
        .await;
    assert_eq!(comp_exp.unwrap_err().code, "ContractExpired");
    // Issuer s1 received back the 5,000 ISK reward + 20,000 ISK forfeited collateral (+2,000,000 cents net over s1_bal_pre)
    assert_eq!(
        ctx.db.get_character(90_000_001).unwrap().balance.0,
        s1_bal_pre + 2_000_000
    );
    // Contracted item 100_000_003 stayed in s2's ship (100_000_002) and transferred ownership to s2 (90_000_002)
    let forfeited_item = ctx.db.get_item(100_000_003).unwrap();
    assert_eq!(forfeited_item.owner_id, 90_000_002);
    assert_eq!(forfeited_item.location_id, 100_000_002);
    assert_eq!(forfeited_item.flag, InventoryFlag::Cargo);

    // 4. R8-P1-4 & R9-P1-3: Accepting missions allocates unique NPC ball IDs per character and mission instance;
    // s1 accepting 909 & 910 and s2 accepting 909 produce 3 distinct NPC balls that never collide.
    let PyRep::KeyVal(u1_kv) = ctx
        .dispatch_rpc(&mut s1, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .unwrap()
    else {
        panic!("Expected u1_kv");
    };
    let s1_b_id = u1_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(&mut s1, "beyonce", Some(s1_b_id), "CmdStop", &[], &kw, None)
        .await
        .unwrap();
    let PyRep::KeyVal(u2_kv) = ctx
        .dispatch_rpc(&mut s2, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .unwrap()
    else {
        panic!("Expected u2_kv");
    };
    let s2_b_id = u2_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(&mut s2, "beyonce", Some(s2_b_id), "CmdStop", &[], &kw, None)
        .await
        .unwrap();
    let mut s1_mids = Vec::new();
    let mut s1_npcs = Vec::new();
    for _ in 0..2 {
        let rm = ctx
            .dispatch_rpc(&mut s1, "agentMgr", None, "DoAction", &[PyRep::Integer(2)], &kw, None)
            .await
            .unwrap();
        let PyRep::KeyVal(rm_kv) = rm else {
            panic!("Expected rm_kv");
        };
        let mid = rm_kv.get("missionID").unwrap().as_i64().unwrap();
        s1_mids.push(mid);
        let am = ctx
            .dispatch_rpc(
                &mut s1,
                "agentMgr",
                None,
                "DoAction",
                &[PyRep::Integer(3), PyRep::Integer(mid)],
                &kw,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(am_kv) = am else {
            panic!("Expected am_kv");
        };
        s1_npcs.push(am_kv.get("spawnedNpcBallID").unwrap().as_i64().unwrap());
    }
    assert_ne!(s1_mids[0], s1_mids[1]);
    let rm_s2 = ctx
        .dispatch_rpc(&mut s2, "agentMgr", None, "DoAction", &[PyRep::Integer(2)], &kw, None)
        .await
        .unwrap();
    let PyRep::KeyVal(rms2_kv) = rm_s2 else {
        panic!("Expected rms2_kv");
    };
    let s2_mid_1 = rms2_kv.get("missionID").unwrap().as_i64().unwrap();
    let am_s2 = ctx
        .dispatch_rpc(
            &mut s2,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(3), PyRep::Integer(s2_mid_1)],
            &kw,
            None,
        )
        .await
        .unwrap();
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
    ctx.dispatch_rpc(
        &mut s1,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(100_000_001),
            PyRep::Integer(27),
            PyRep::Integer(100_000_942),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut s2,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(100_000_002),
            PyRep::Integer(27),
            PyRep::Integer(100_000_943),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();

    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    for _ in 0..4 {
        let _ = jita_handle.step_manual(1.0).await.unwrap();
    }
    // Third-party pilot (`s2`) cannot attack `s1`'s instanced mission pocket NPC
    let cross_fire = ctx
        .dispatch_rpc(
            &mut s2,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(s1_npcs[0])],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        cross_fire.unwrap_err().code,
        "MissionPocketInstanceRestricted"
    );

    let mut fw1_destroyed = false;
    for shot in 0..10 {
        if shot > 0 {
            for _ in 0..4 {
                let _ = jita_handle.step_manual(1.0).await.unwrap();
            }
        }
        let fw1 = ctx
            .dispatch_rpc(
                &mut s1,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(s1_npcs[0])],
                &kw,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(fw1_kv) = fw1 else {
            panic!("Expected fw1_kv");
        };
        if fw1_kv.get("destroyed") == Some(&PyRep::Bool(true)) {
            fw1_destroyed = true;
            break;
        }
    }
    assert!(fw1_destroyed);

    let gate_m2_early = ctx
        .dispatch_rpc(
            &mut s1,
            "keeper",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(s1_mids[1])],
            &kw,
            None,
        )
        .await;
    assert_eq!(gate_m2_early.unwrap_err().code, "RoomNpcsRemaining");
    let m2_early = ctx
        .dispatch_rpc(
            &mut s1,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(6), PyRep::Integer(s1_mids[1])],
            &kw,
            None,
        )
        .await;
    assert_eq!(m2_early.unwrap_err().code, "MissionIncomplete");

    let mut fw2_destroyed = false;
    for _ in 0..10 {
        for _ in 0..4 {
            let _ = jita_handle.step_manual(1.0).await.unwrap();
        }
        let fw2 = ctx
            .dispatch_rpc(
                &mut s1,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(s1_npcs[1])],
                &kw,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(fw2_kv) = fw2 else {
            panic!("Expected fw2_kv");
        };
        if fw2_kv.get("destroyed") == Some(&PyRep::Bool(true)) {
            fw2_destroyed = true;
            break;
        }
    }
    assert!(fw2_destroyed);
    ctx.dispatch_rpc(
        &mut s1,
        "keeper",
        None,
        "ActivateAccelerationGate",
        &[PyRep::Integer(s1_mids[1])],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut s1,
        "agentMgr",
        None,
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(s1_mids[1])],
        &kw,
        None,
    )
    .await
    .unwrap();

    // s2's mission NPC is still alive and independent; s2 destroys it and completes s2_mid_1
    let mut fws2_destroyed = false;
    for shot in 0..10 {
        if shot > 0 {
            for _ in 0..4 {
                let _ = jita_handle.step_manual(1.0).await.unwrap();
            }
        }
        let fw_s2 = ctx
            .dispatch_rpc(
                &mut s2,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(s2_npc_1001)],
                &kw,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(fws2_kv) = fw_s2 else {
            panic!("Expected fws2_kv");
        };
        if fws2_kv.get("destroyed") == Some(&PyRep::Bool(true)) {
            fws2_destroyed = true;
            break;
        }
    }
    assert!(fws2_destroyed);
    ctx.dispatch_rpc(
        &mut s2,
        "keeper",
        None,
        "ActivateAccelerationGate",
        &[PyRep::Integer(s2_mid_1)],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut s2,
        "agentMgr",
        None,
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(s2_mid_1)],
        &kw,
        None,
    )
    .await
    .unwrap();

    // Mission abandon/decline via RemoveOfferFromJournal despawns the pocket NPC and clears the journal entry
    let rm_1003 = ctx
        .dispatch_rpc(&mut s1, "agentMgr", None, "DoAction", &[PyRep::Integer(2)], &kw, None)
        .await
        .unwrap();
    let PyRep::KeyVal(rm1003_kv) = rm_1003 else {
        panic!("Expected rm1003_kv");
    };
    let s1_mid_3 = rm1003_kv.get("missionID").unwrap().as_i64().unwrap();
    let am_1003 = ctx
        .dispatch_rpc(
            &mut s1,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(3), PyRep::Integer(s1_mid_3)],
            &kw,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(am1003_kv) = am_1003 else {
        panic!("Expected am1003_kv");
    };
    let npc_1003 = am1003_kv.get("spawnedNpcBallID").unwrap().as_i64().unwrap();
    assert!(jita_handle
        .get_ball_snapshot(npc_1003)
        .await
        .unwrap()
        .is_some());
    ctx.dispatch_rpc(
        &mut s1,
        "agentMgr",
        None,
        "RemoveOfferFromJournal",
        &[PyRep::Integer(s1_mid_3)],
        &kw,
        None,
    )
    .await
    .unwrap();
    assert!(jita_handle
        .get_ball_snapshot(npc_1003)
        .await
        .unwrap()
        .is_none());

    // N1, N2 & R36-P1-7: Cannot abandon an already completed or already quit mission, cannot re-accept either, and client cannot supply mission_id to DoAction(RequestMission)
    let quit_completed = ctx
        .dispatch_rpc(
            &mut s1,
            "agentMgr",
            None,
            "RemoveOfferFromJournal",
            &[PyRep::Integer(s1_mids[1])],
            &kw,
            None,
        )
        .await;
    assert_eq!(quit_completed.unwrap_err().code, "MissionNotActive");
    let quit_already_quit = ctx
        .dispatch_rpc(
            &mut s1,
            "agentMgr",
            None,
            "RemoveOfferFromJournal",
            &[PyRep::Integer(s1_mid_3)],
            &kw,
            None,
        )
        .await;
    assert_eq!(quit_already_quit.unwrap_err().code, "MissionNotActive");
    let reaccept_completed = ctx
        .dispatch_rpc(
            &mut s1,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(3), PyRep::Integer(s1_mids[1])],
            &kw,
            None,
        )
        .await;
    assert_eq!(reaccept_completed.unwrap_err().code, "InvalidMissionState");
    let client_supplied_mid = ctx
        .dispatch_rpc(
            &mut s1,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(2), PyRep::Integer(s1_mids[1])],
            &kw,
            None,
        )
        .await;
    assert_eq!(client_supplied_mid.unwrap_err().code, "UnknownAgent");

    // N4: Cannot fit a module into a ship slot while in space (`CannotFitInSpace`)
    let fit_in_space = ctx
        .dispatch_rpc(
            &mut s1,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_000_003),
                PyRep::Integer(100_000_001),
                PyRep::Integer(12),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(fit_in_space.unwrap_err().code, "CannotFitInSpace");

    // N7: Cannot warp to arbitrary unscanned/unbookmarked coordinates (`InvalidWarpDestination`)
    let s1_bound = ctx.bind_object(&mut s1, "beyonce", 30_000_142).unwrap();
    let invalid_warp = ctx
        .dispatch_rpc(
            &mut s1,
            "beyonce",
            Some(s1_bound),
            "CmdWarpToStuff",
            &[
                PyRep::Real(900_000_000_000.0),
                PyRep::Real(900_000_000_000.0),
                PyRep::Real(900_000_000_000.0),
            ],
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
    let warp_to_other_ship = ctx
        .dispatch_rpc(
            &mut s1,
            "beyonce",
            Some(s1_bound),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(100_000_002)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        warp_to_other_ship.unwrap_err().code,
        "InvalidWarpDestination"
    );

    // R12-P1-3: CmdWarpToStuff resolves ("item", stargate_id) and rejects invalid targets
    let warp_stuff_ok = ctx
        .dispatch_rpc(
            &mut s1,
            "beyonce",
            Some(s1_bound),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(50_001_249)],
            &kw,
            None,
        )
        .await;
    assert!(warp_stuff_ok.is_ok(), "CmdWarpToStuff to stargate 50001249");
    let warp_stuff_bad = ctx
        .dispatch_rpc(
            &mut s1,
            "beyonce",
            Some(s1_bound),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(999_999_999)],
            &kw,
            None,
        )
        .await;
    assert_eq!(warp_stuff_bad.unwrap_err().code, "InvalidWarpDestination");

    // R12-P1-1: Real SDE type 3520 (Heavy Pulse Laser II) has no fabricated capital damage (114..=118), and T2 BPO (11372) / rolled Abyssal (47749) are not sold on NPC market
    assert!(!ctx.sde.types[&3520].default_attributes.contains_key(&114));
    let (orders_11372, orders_47749) = ctx.db.with_snapshot(|snap| {
        (
            snap.market_orders
                .values()
                .filter(|o| o.type_id == 11372)
                .count(),
            snap.market_orders
                .values()
                .filter(|o| o.type_id == 47749)
                .count(),
        )
    });
    assert_eq!(orders_11372, 0);
    assert_eq!(orders_47749, 0);


    let (s1, s2) = crate::regressions_r8_part2::run_r8_part2(&ctx, s1, s2).await;
    crate::regressions_r8_part3::run_r8_part3(&ctx, s1, s2).await;
}
