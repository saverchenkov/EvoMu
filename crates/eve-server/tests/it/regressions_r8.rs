//! Integration regression tests covering Round 8 findings.


use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_review_r8_regressions() {
    use eve_db::InventoryFlag;

    let ctx = ServerContext::new();
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
            "FireWeapon", // rpc-lint: allow legacy/negative-test RPC (dogmaIM.FireWeapon)
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
                "FireWeapon", // rpc-lint: allow legacy/negative-test RPC (dogmaIM.FireWeapon)
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
                "FireWeapon", // rpc-lint: allow legacy/negative-test RPC (dogmaIM.FireWeapon)
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
                "FireWeapon", // rpc-lint: allow legacy/negative-test RPC (dogmaIM.FireWeapon)
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
            "CmdWarpToPoint", // rpc-lint: allow legacy/negative-test RPC (beyonce.CmdWarpToPoint)
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
            "CmdWarpToPoint", // rpc-lint: allow legacy/negative-test RPC (beyonce.CmdWarpToPoint)
            &[
                PyRep::Real(500_000_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
            ],
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
}
