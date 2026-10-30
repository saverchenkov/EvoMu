//! End-to-end integration tests for Batch 22 corporation governance, projects, and NPC corp rules.

#![forbid(unsafe_code)]

use eve_db::IskCents;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

use super::open_feats_and_rpcs_b22::{field, rowset_rows, rpc, select_pilot, PILOT_1, PILOT_2};

#[tokio::test]
async fn test_b22_corp_governance_shares_dividends_ceo_divisions_autokick() {
    // parity: feat:corporation.ceo-resign, feat:corporation.corp-division-names, feat:corporation.dividends, feat:corporation.pending-auto-kick, feat:corporation.share-transfer
    eve_testkit::attest(&[
        "feat:corporation.ceo-resign",
        "feat:corporation.corp-division-names",
        "feat:corporation.dividends",
        "feat:corporation.pending-auto-kick",
        "feat:corporation.share-transfer",
    ]);

    let ctx = ServerContext::new();
    let mut sess1 = select_pilot(&ctx, 72_201, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 72_202, "other", "other-secret", PILOT_2).await;

    let created = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Sovereign Syndicate".into()),
            PyRep::String("SVRN".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID").as_i64().unwrap();

    let dock_id = sess2.station_id.unwrap_or(60_003_760);
    ctx.db.with_mut_snapshot(|s| {
        let corp = s.corporations.get_mut(&corp_id).unwrap();
        corp.members.insert(PILOT_2);
        corp.ensure_wallet_divisions();
        corp.master_wallet = IskCents::from_f64_isk(10_000_000.0);
        corp.wallet_divisions[0].balance = IskCents::from_f64_isk(10_000_000.0);
        s.characters.get_mut(&PILOT_2).unwrap().corporation_id = corp_id;
        s.characters.get_mut(&PILOT_2).unwrap().balance = IskCents::ZERO;
        s.next_entity_id += 1;
        let item_id = s.next_entity_id;
        s.items.insert(
            item_id,
            eve_db::InventoryItemRecord {
                item_id,
                type_id: 34,
                owner_id: PILOT_2,
                location_id: dock_id,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 10,
                ..Default::default()
            },
        );
    });
    sess2.corporation_id = Some(corp_id);

    // 1. Division names
    let deny_div = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "UpdateDivisionNames",
        &[PyRep::Integer(1), PyRep::String("Hack".into()), PyRep::String("HackW".into())],
    )
    .await;
    assert!(deny_div.is_err(), "Non-director cannot rename divisions");

    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateDivisionNames",
        &[PyRep::Integer(1), PyRep::String("Armory".into()), PyRep::String("Treasury".into())],
    )
    .await
    .expect("UpdateDivisionNames");
    let divs = rpc(&ctx, &mut sess1, "corpRegistry", "GetDivisionNames", &[PyRep::Integer(corp_id)])
        .await
        .expect("GetDivisionNames");
    assert_eq!(field(&divs, "division1"), PyRep::String("Armory".into()));
    assert_eq!(field(&divs, "walletDivision1"), PyRep::String("Treasury".into()));

    // 2. Share transfers (MoveCompanyShares + MovePrivateShares) & exact share counts
    let deny_shares = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "MoveCompanyShares",
        &[PyRep::Integer(corp_id), PyRep::Integer(PILOT_2), PyRep::Integer(100)],
    )
    .await;
    assert_eq!(deny_shares.unwrap_err().code, "CorpRoleDenied");

    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "MoveCompanyShares",
        &[PyRep::Integer(corp_id), PyRep::Integer(PILOT_2), PyRep::Integer(400)],
    )
    .await
    .expect("MoveCompanyShares");

    let sh_p2 = rpc(&ctx, &mut sess1, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(PILOT_2)])
        .await
        .expect("GetSharesByShareholder PILOT_2");
    assert_eq!(
        rowset_rows(&sh_p2),
        vec![vec![PyRep::Integer(PILOT_2), PyRep::Integer(corp_id), PyRep::Integer(400)]]
    );

    rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "MovePrivateShares",
        &[PyRep::Integer(corp_id), PyRep::Integer(PILOT_1), PyRep::Integer(150)],
    )
    .await
    .expect("MovePrivateShares");

    let sh_p2_after = rpc(&ctx, &mut sess1, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(PILOT_2)])
        .await
        .expect("GetSharesByShareholder PILOT_2 after");
    assert_eq!(
        rowset_rows(&sh_p2_after),
        vec![vec![PyRep::Integer(PILOT_2), PyRep::Integer(corp_id), PyRep::Integer(250)]]
    );
    let sh_p1_after = rpc(&ctx, &mut sess1, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(PILOT_1)])
        .await
        .expect("GetSharesByShareholder PILOT_1 after");
    assert_eq!(
        rowset_rows(&sh_p1_after),
        vec![vec![PyRep::Integer(PILOT_1), PyRep::Integer(corp_id), PyRep::Integer(150)]]
    );

    // 3. PayoutDividend (negative cases + exact amounts + wallet journals on both sides)
    let deny_div_pay = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "PayoutDividend",
        &[PyRep::Bool(true), PyRep::Real(1_000_000.0), PyRep::Integer(1)],
    )
    .await;
    assert_eq!(deny_div_pay.unwrap_err().code, "CorpRoleDenied");

    let insuf_div_pay = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "PayoutDividend",
        &[PyRep::Bool(true), PyRep::Real(50_000_000.0), PyRep::Integer(1)],
    )
    .await;
    assert_eq!(insuf_div_pay.unwrap_err().code, "InsufficientFunds");

    let p1_bal_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance.raw_cents());
    rpc(&ctx, &mut sess1, "corpRegistry", "PayoutDividend", &[PyRep::Bool(true), PyRep::Real(1_000_000.0), PyRep::Integer(1)])
        .await
        .expect("PayoutDividend");
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.characters[&PILOT_2].balance.raw_cents(), 250_000_00, "PILOT_2 (250/1000 shares) must receive 250,000 ISK");
        assert_eq!(s.characters[&PILOT_1].balance.raw_cents() - p1_bal_before, 150_000_00, "PILOT_1 (150/1000 shares) must receive 150,000 ISK");
        assert_eq!(s.corporations[&corp_id].division_balance(1).raw_cents(), 9_600_000_00, "Corp wallet must be debited by 400,000 ISK");
        assert!(s.wallet_journal.iter().any(|j| j.character_id == PILOT_2 && j.entry_type == "CorporationDividendPayment" && j.amount.raw_cents() == 250_000_00));
        assert!(s.wallet_journal.iter().any(|j| j.character_id == PILOT_1 && j.entry_type == "CorporationDividendPayment" && j.amount.raw_cents() == 150_000_00));
        assert!(s.wallet_journal.iter().any(|j| j.character_id == corp_id && j.entry_type == "CorporationDividendPayout" && j.amount.raw_cents() == -400_000_00));
    });

    // 4. CEO resignation gating (GetNumberOfPotentialCEOs + multi-member vote/successor requirement)
    let pot_ceos = rpc(&ctx, &mut sess1, "corpRegistry", "GetNumberOfPotentialCEOs", &[])
        .await
        .expect("GetNumberOfPotentialCEOs");
    assert_eq!(pot_ceos, PyRep::Integer(1));
    let deny_resign = rpc(&ctx, &mut sess1, "corpRegistry", "ResignFromCEO", &[]).await;
    assert_eq!(deny_resign.unwrap_err().code, "InvalidNewCEO");

    // 5. Pending auto-kick when member is in space
    ctx.advance_secs(12);
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&PILOT_2).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
    });
    rpc(&ctx, &mut sess1, "corpRegistry", "KickOutMember", &[PyRep::Integer(PILOT_2)])
        .await
        .expect("KickOutMember queued");
    let pending = rpc(&ctx, &mut sess1, "corpRegistry", "GetPendingAutoKicks", &[])
        .await
        .expect("GetPendingAutoKicks");
    assert!(rowset_rows(&pending).iter().any(|r| r.first() == Some(&PyRep::Integer(PILOT_2))));

    let executed = eve_server::services::fleet_social::execute_pending_auto_kicks_on_downtime(&ctx);
    assert!(executed >= 1);
    ctx.db.with_snapshot(|s| {
        assert!(!s.corporations[&corp_id].members.contains(&PILOT_2));
    });

    // 6. Solo CEO resignation -> vacant CEO state (newCeoID == 0)
    let pot_ceos_after = rpc(&ctx, &mut sess1, "corpRegistry", "GetNumberOfPotentialCEOs", &[])
        .await
        .expect("GetNumberOfPotentialCEOs after kick");
    assert_eq!(pot_ceos_after, PyRep::Integer(0));
    let resign = rpc(&ctx, &mut sess1, "corpRegistry", "ResignFromCEO", &[])
        .await
        .expect("ResignFromCEO");
    assert_eq!(field(&resign, "newCeoID"), PyRep::Integer(0));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.corporations[&corp_id].ceo_id, 0);
    });
}

#[tokio::test]
async fn test_b22_corp_projects_and_npc_corp_rules() {
    // parity: feat:corporation.corp-project-close-expiry, feat:corporation.corp-project-contribution-methods, feat:corporation.corp-project-reward-pool, feat:corporation.npc-corp-placement-rules, feat:corporation.npc-corp-tax-rate
    eve_testkit::attest(&[
        "feat:corporation.corp-project-close-expiry",
        "feat:corporation.corp-project-contribution-methods",
        "feat:corporation.corp-project-reward-pool",
        "feat:corporation.npc-corp-placement-rules",
        "feat:corporation.npc-corp-tax-rate",
    ]);

    let ctx = ServerContext::new();
    let mut sess1 = select_pilot(&ctx, 72_301, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 72_302, "other", "other-secret", PILOT_2).await;

    let created = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Helios Extraction".into()),
            PyRep::String("HLOS".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID").as_i64().unwrap();

    let dock_id = sess2.station_id.unwrap_or(60_003_760);
    ctx.db.with_mut_snapshot(|s| {
        let corp = s.corporations.get_mut(&corp_id).unwrap();
        corp.members.insert(PILOT_2);
        corp.ensure_wallet_divisions();
        corp.master_wallet = IskCents::from_f64_isk(10_000_000.0);
        corp.wallet_divisions[0].balance = IskCents::from_f64_isk(10_000_000.0);
        s.characters.get_mut(&PILOT_2).unwrap().corporation_id = corp_id;
        s.characters.get_mut(&PILOT_2).unwrap().balance = IskCents::ZERO;
        s.next_entity_id += 1;
        let item_id = s.next_entity_id;
        s.items.insert(
            item_id,
            eve_db::InventoryItemRecord {
                item_id,
                type_id: 34,
                owner_id: PILOT_2,
                location_id: dock_id,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 10,
                ..Default::default()
            },
        );
    });
    sess2.corporation_id = Some(corp_id);

    // 1. Create projects for MineMaterials, DestroyNPCShips, EarnLoyaltyPoints, and DeliverItem
    let create_mine = rpc(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "CreateGoalSubmit",
        &[
            PyRep::String("Mine Veldspar".into()),
            PyRep::Integer(100),
            PyRep::Real(10_000.0),
            PyRep::String("MineMaterials".into()),
            PyRep::Bool(false),
            PyRep::Integer(1),
            PyRep::Integer(50),
            PyRep::Integer(86_400),
        ],
    )
    .await
    .expect("CreateGoalSubmit MineMaterials");
    let pid_mine = field(&create_mine, "projectID").as_i64().unwrap();

    let create_npc = rpc(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "CreateGoalSubmit",
        &[
            PyRep::String("Clear Rats".into()),
            PyRep::Integer(10),
            PyRep::Real(5_000.0),
            PyRep::String("DestroyNPCShips".into()),
            PyRep::Bool(false),
            PyRep::Integer(1),
            PyRep::Integer(10),
            PyRep::Integer(86_400),
        ],
    )
    .await
    .expect("CreateGoalSubmit DestroyNPCShips");
    let pid_npc = field(&create_npc, "projectID").as_i64().unwrap();

    let create_lp = rpc(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "CreateGoalSubmit",
        &[
            PyRep::String("Earn LP".into()),
            PyRep::Integer(100),
            PyRep::Real(1_000.0),
            PyRep::String("EarnLoyaltyPoints".into()),
            PyRep::Bool(false),
            PyRep::Integer(1),
            PyRep::Integer(100),
            PyRep::Integer(86_400),
        ],
    )
    .await
    .expect("CreateGoalSubmit EarnLoyaltyPoints");
    let pid_lp = field(&create_lp, "projectID").as_i64().unwrap();

    let create_deliv = rpc(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "CreateGoalSubmit",
        &[
            PyRep::String("Deliver Tritanium".into()),
            PyRep::Integer(50),
            PyRep::Real(1_000.0),
            PyRep::String("DeliverItem".into()),
            PyRep::Bool(false),
            PyRep::Integer(1),
            PyRep::Integer(50),
            PyRep::Integer(86_400),
        ],
    )
    .await
    .expect("CreateGoalSubmit DeliverItem");
    let pid_deliv = field(&create_deliv, "projectID").as_i64().unwrap();

    // Client-reported GoalProgress on server-tracked MineMaterials must be rejected
    let client_spoof = rpc(&ctx, &mut sess2, "corporationSvc", "GoalProgress", &[PyRep::Integer(pid_mine), PyRep::Integer(10)]).await;
    assert_eq!(client_spoof.unwrap_err().code, "ServerTrackedMatcher");

    // Drive real server paths for MineMaterials, DestroyNPCShips, EarnLoyaltyPoints, and DeliverItem
    eve_server::services::space_mining_harvest::dispatch_mining_gameplay_events(&ctx, PILOT_2, 10, 30000142, 1230);
    eve_server::services::combat_npc_encounters::record_npc_kill_progression(&ctx, PILOT_2, Some(10001));
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        eve_server::services::lp_store::record_lp_reward_log(s, PILOT_2, 1_000_006, 25, "LP reward".into(), now_unix);
    });
    rpc(&ctx, &mut sess2, "corporationSvc", "GoalProgress", &[PyRep::Integer(pid_deliv), PyRep::Integer(5)])
        .await
        .expect("GoalProgress DeliverItem");

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.corp_projects[&pid_mine].current_quantity, 10);
        assert_eq!(s.corp_projects[&pid_npc].current_quantity, 1);
        assert_eq!(s.corp_projects[&pid_lp].current_quantity, 25);
        assert_eq!(s.corp_projects[&pid_deliv].current_quantity, 5);
        assert_eq!(s.characters[&PILOT_2].balance.raw_cents(), 135_000_00);
    });

    // Close project and verify remaining escrow (900,000 ISK) refunded
    let bal_before_close = ctx.db.with_snapshot(|s| s.corporations[&corp_id].wallet_divisions[0].balance.raw_cents());
    rpc(&ctx, &mut sess1, "corporationSvc", "CloseCorpProject", &[PyRep::Integer(pid_mine)])
        .await
        .expect("CloseCorpProject");
    ctx.db.with_snapshot(|s| {
        let bal_after_close = s.corporations[&corp_id].wallet_divisions[0].balance.raw_cents();
        assert_eq!(bal_after_close - bal_before_close, 900_000_00);
    });

    // 2. NPC corp placement rules & 11% NPC corp tax rate
    let npc_app = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(1_000_006), PyRep::String("Join NPC".into())],
    )
    .await;
    assert!(npc_app.is_err(), "Cannot apply to NPC corporation");

    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_2).unwrap().corporation_id = 1_000_006;
    });
    let sys_id = 30000142;
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((sys_id, PILOT_2), 50_000.0);
    });
    let (small_net, _, _) = eve_server::services::combat_npc_encounters::flush_npc_bounty_tick(&ctx, sys_id, PILOT_2, 0.0);
    assert!((small_net - 67_500.0).abs() < 1e-3, "Bounty under 100k ISK must be tax-exempt");

    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((sys_id, PILOT_2), 1_000_000.0);
    });
    let (large_net, _, _) = eve_server::services::combat_npc_encounters::flush_npc_bounty_tick(&ctx, sys_id, PILOT_2, 0.0);
    assert!((large_net - 1_201_500.0).abs() < 1e-3, "11% NPC corp tax must apply above 100k ISK threshold");
    ctx.db.with_snapshot(|s| {
        assert!(
            s.wallet_journal.iter().any(|j| j.character_id == PILOT_2 && j.entry_type == "bounty_prize_corporation_tax" && j.amount.raw_cents() == -148_500_00),
            "Wallet journal must record 11% NPC corp tax deduction"
        );
    });
}
