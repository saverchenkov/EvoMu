//! End-to-end integration tests for Batch 22 corporation governance, projects, and NPC corp rules.

#![forbid(unsafe_code)]

use eve_db::IskCents;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

use super::features_market_orders_rpcs::{field, rowset_rows, rpc, select_pilot, PILOT_1, PILOT_2};

#[tokio::test]
async fn test_b22_corp_governance_shares_dividends_ceo_divisions_autokick() {
    // parity: feat:corporation.ceo-resign, feat:corporation.corp-division-names, feat:corporation.dividends, feat:corporation.share-transfer
    eve_testkit::attest(&[
        "feat:corporation.ceo-resign",
        "feat:corporation.corp-division-names",
        "feat:corporation.dividends",
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
    let mut trit_item_id = 0_i64;
    ctx.db.with_mut_snapshot(|s| {
        let corp = s.corporations.get_mut(&corp_id).unwrap();
        corp.members.insert(PILOT_2);
        corp.ensure_wallet_divisions();
        corp.master_wallet = IskCents::from_f64_isk(10_000_000.0);
        corp.wallet_divisions[0].balance = IskCents::from_f64_isk(10_000_000.0);
        s.characters.get_mut(&PILOT_2).unwrap().corporation_id = corp_id;
        s.characters.get_mut(&PILOT_2).unwrap().balance = IskCents::ZERO;
        s.next_entity_id += 1;
        trit_item_id = s.next_entity_id;
        s.items.insert(
            trit_item_id,
            eve_db::InventoryItemRecord {
                item_id: trit_item_id,
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

    // 1. Division names (V24 client 14-argument call shape + GetDivisionNames + ESI divisions)
    let hangar_names = [
        "Armory", "Ore Bay", "Components", "Ammo", "Salvage", "Blueprints", "Capitals",
    ];
    let wallet_names = [
        "Treasury", "SRP", "Industry", "Fuel", "Taxes", "Bounties", "Reserve",
    ];
    let div_args_14: Vec<PyRep> = hangar_names
        .iter()
        .chain(wallet_names.iter())
        .map(|s| PyRep::String((*s).into()))
        .collect();
    let deny_div = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "UpdateDivisionNames",
        &div_args_14,
    )
    .await;
    assert_eq!(deny_div.unwrap_err().code, "CorpRoleDenied", "Non-director cannot rename divisions");

    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateDivisionNames",
        &div_args_14,
    )
    .await
    .expect("UpdateDivisionNames (14 args)");
    let divs = rpc(&ctx, &mut sess1, "corpRegistry", "GetDivisionNames", &[PyRep::Integer(corp_id)])
        .await
        .expect("GetDivisionNames");
    for i in 0..7 {
        assert_eq!(field(&divs, &format!("division{}", i + 1)), PyRep::String(hangar_names[i].into()));
        assert_eq!(field(&divs, &format!("walletDivision{}", i + 1)), PyRep::String(wallet_names[i].into()));
    }
    let mut esi_params = HashMap::new();
    esi_params.insert("corporation_id".to_string(), corp_id.to_string());
    let esi_divs = eve_server::esi::sample_routes::dispatch_operation(
        &ctx,
        ctx.now_unix(),
        "GET",
        "/corporations/{corporation_id}/divisions",
        &esi_params,
        &HashMap::new(),
        None,
        1,
        &[],
    );
    assert_eq!(esi_divs.status_code, 200, "ESI divisions route failed: {esi_divs:?}");
    let esi_body = esi_divs.body.expect("ESI divisions JSON");
    assert_eq!(esi_body["hangar"][0]["name"], "Armory");
    assert_eq!(esi_body["hangar"][6]["name"], "Capitals");
    assert_eq!(esi_body["wallet"][0]["name"], "Treasury");
    assert_eq!(esi_body["wallet"][6]["name"], "Reserve");

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

    // 4. CEO candidacy (CanRunForCEO), resignation gating, and CEO election via vote case
    ctx.db.with_mut_snapshot(|s| {
        if let Some(sk) = s.skills.get_mut(&PILOT_2) {
            sk.remove(&3363);
        }
    });
    assert_eq!(
        rpc(&ctx, &mut sess2, "voteManager", "CanRunForCEO", &[]).await.unwrap(),
        PyRep::Bool(false),
        "CanRunForCEO must be false without Corporation Management skill (3363)"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(PILOT_2).or_default().insert(
            3363,
            eve_db::TrainedSkillRecord { type_id: 3363, level: 1, skill_points: 250.0 },
        );
    });
    assert_eq!(
        rpc(&ctx, &mut sess2, "voteManager", "CanRunForCEO", &[]).await.unwrap(),
        PyRep::Bool(true),
        "CanRunForCEO must be true with Corporation Management skill (3363)"
    );

    let pot_ceos = rpc(&ctx, &mut sess1, "corpRegistry", "GetNumberOfPotentialCEOs", &[])
        .await
        .expect("GetNumberOfPotentialCEOs");
    assert_eq!(pot_ceos, PyRep::Integer(1));
    let deny_resign = rpc(&ctx, &mut sess1, "corpRegistry", "ResignFromCEO", &[]).await;
    assert_eq!(deny_resign.unwrap_err().code, "InvalidNewCEO");

    // Elect PILOT_2 as CEO via CEO vote case (vote_type = 0)
    let vc_rep = rpc(
        &ctx,
        &mut sess1,
        "voteManager",
        "InsertVoteCase",
        &[
            PyRep::Integer(corp_id),
            PyRep::String("Elect PILOT_2 as CEO".into()),
            PyRep::String("CEO succession vote".into()),
            PyRep::Integer(0),
            PyRep::Integer(PILOT_2),
        ],
    )
    .await
    .expect("InsertVoteCase (CEO election)");
    let case_id = field(&vc_rep, "voteCaseID").as_i64().unwrap();
    rpc(
        &ctx,
        &mut sess2,
        "voteManager",
        "InsertVote",
        &[PyRep::Integer(corp_id), PyRep::Integer(case_id), PyRep::Integer(1)],
    )
    .await
    .expect("InsertVote for PILOT_2");
    ctx.advance_secs(86_401);
    rpc(
        &ctx,
        &mut sess2,
        "voteManager",
        "ActivateSanctionedAction",
        &[PyRep::Integer(case_id)],
    )
    .await
    .expect("ActivateSanctionedAction (CEO election)");
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.corporations[&corp_id].ceo_id, PILOT_2, "Vote case must elect PILOT_2 as CEO");
    });

    // PILOT_2 resigns and designates PILOT_1 as successor
    let handback = rpc(&ctx, &mut sess2, "corpRegistry", "ResignFromCEO", &[PyRep::Integer(PILOT_1)])
        .await
        .expect("ResignFromCEO(PILOT_1)");
    assert_eq!(field(&handback, "newCeoID"), PyRep::Integer(PILOT_1));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.corporations[&corp_id].ceo_id, PILOT_1);
    });

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
    // parity: feat:corporation.corp-project-close-expiry, feat:corporation.corp-project-reward-pool, feat:esi.corporation.projects
    eve_testkit::attest(&[
        "feat:corporation.corp-project-close-expiry",
        "feat:corporation.corp-project-reward-pool",
        "feat:esi.corporation.projects",
    ]);

    let ctx = std::sync::Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
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
    let mut trit_item_id = 0_i64;
    ctx.db.with_mut_snapshot(|s| {
        let corp = s.corporations.get_mut(&corp_id).unwrap();
        corp.members.insert(PILOT_2);
        corp.ensure_wallet_divisions();
        corp.master_wallet = IskCents::from_f64_isk(10_000_000.0);
        corp.wallet_divisions[0].balance = IskCents::from_f64_isk(10_000_000.0);
        s.characters.get_mut(&PILOT_2).unwrap().corporation_id = corp_id;
        s.characters.get_mut(&PILOT_2).unwrap().balance = IskCents::ZERO;
        s.next_entity_id += 1;
        trit_item_id = s.next_entity_id;
        s.items.insert(
            trit_item_id,
            eve_db::InventoryItemRecord {
                item_id: trit_item_id,
                type_id: 34,
                owner_id: PILOT_2,
                location_id: dock_id,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 5,
                ..Default::default()
            },
        );
    });
    sess2.corporation_id = Some(corp_id);

    let q_req = |sess: &eve_machonet::SessionState, type_name: &str, payload: Vec<u8>| {
        let now_u = ctx.now_unix();
        let req = eve_proto::quasar::EvePublicRequest {
            issued: Some(eve_proto::quasar::GoogleTimestamp { seconds: now_u as i64, nanos: 0 }),
            correlation_uuid: vec![0x51; 16],
            payload: Some(eve_proto::quasar::GoogleAny { type_url: format!("type.googleapis.com/{type_name}"), value: payload }),
            external_origin: "test".into(),
            application_instance_uuid: vec![0x52; 16],
            authoritative_context: Some(
                eve_proto::quasar::AuthoritativeContext::for_session_at(
                    sess.user_id.map(|u| u as i32),
                    sess.character_id.map(|c| c as u32),
                    sess.corporation_id.map(|c| c as u32),
                    sess.corp_role,
                    "tranquility",
                    vec![0x53; 16],
                    now_u,
                )
                .with_service_roles(sess.role),
            ),
            trace_context: None,
        };
        ctx.quasar_gateway.handle_public_request(sess, &req, None, false, now_u)
    };
    let create_goal = |name: &str, target: u64, escrow_isk: i64, cfg_field: u32, item_filter: Option<u64>, limit: u64| -> (i64, Vec<u8>) {
        let (mut m, mut cfg, mut curr, mut isk, mut pool, mut ts) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        eve_proto::quasar::encode_string_field(1, name, &mut m);
        eve_proto::quasar::encode_varint_field(3, target, &mut m);
        let sub_cfg = if let Some(tid) = item_filter {
            let (mut id_b, mut card, mut sets, mut items, mut deliv) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
            eve_proto::quasar::encode_varint_field(1, tid, &mut id_b);
            eve_proto::quasar::encode_bytes_field(1, &id_b, &mut card);
            eve_proto::quasar::encode_bytes_field(3, &card, &mut sets);
            eve_proto::quasar::encode_bytes_field(2, &sets, &mut items);
            eve_proto::quasar::encode_bytes_field(1, &items, &mut deliv);
            deliv
        } else {
            Vec::new()
        };
        eve_proto::quasar::encode_bytes_field(cfg_field, &sub_cfg, &mut cfg);
        eve_proto::quasar::encode_bytes_field(4, &cfg, &mut m);
        eve_proto::quasar::encode_varint_field(1, eve_proto::quasar::encode_zigzag64(escrow_isk), &mut curr);
        eve_proto::quasar::encode_bytes_field(1, &curr, &mut isk);
        eve_proto::quasar::encode_varint_field(1, 1, &mut pool);
        eve_proto::quasar::encode_bytes_field(2, &isk, &mut pool);
        eve_proto::quasar::encode_bytes_field(8, &pool, &mut m);
        eve_proto::quasar::encode_varint_field(1, ctx.now_unix() + 86_400, &mut ts);
        eve_proto::quasar::encode_bytes_field(10, &ts, &mut m);
        eve_proto::quasar::encode_varint_field(26, limit, &mut m);
        let rsp = q_req(&sess1, "eve_public.goal.api.CreateRequest", m);
        assert_eq!(rsp.status_code, eve_server::quasar_gateway::STATUS_OK, "{}", rsp.status_message);
        let id_wire = rsp.payload.unwrap().value;
        let pid = eve_server::services::social_corp_fleet::social_corp_projects::goal_uuid_to_project_id(&id_wire[4..20]).unwrap();
        (pid, id_wire)
    };

    // 1. Create projects via Quasar for MineMaterials (25), DestroyNPCShips (31), EarnLoyaltyPoints (29), and DeliverItem (37)
    let (pid_mine, mine_id_wire) = create_goal("Mine Veldspar", 100, 1_000_000, 25, None, 50);
    let (pid_npc, _) = create_goal("Clear Rats", 10, 50_000, 31, None, 10);
    let (pid_lp, _) = create_goal("Earn LP", 100, 100_000, 29, None, 100);
    let (pid_deliv, _) = create_goal("Deliver Tritanium", 50, 50_000, 37, Some(34), 50);

    // Client-reported SetCurrentProgressRequest from non-GM member must be rejected
    let mut spoof_req = mine_id_wire.clone();
    eve_proto::quasar::encode_varint_field(3, 10, &mut spoof_req);
    assert_eq!(q_req(&sess2, "eve_public.goal.api.SetCurrentProgressRequest", spoof_req).status_code, eve_server::quasar_gateway::STATUS_FORBIDDEN);

    // Drive real server paths for MineMaterials, DestroyNPCShips, EarnLoyaltyPoints, and DeliverItem
    eve_server::services::space_mining_harvest::dispatch_mining_gameplay_events(&ctx, PILOT_2, 10, 30000142, 1230);
    eve_server::services::combat_npc_encounters::record_npc_kill_progression(&ctx, PILOT_2, Some(10001));
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        eve_server::services::lp_store::record_lp_reward_log(s, PILOT_2, 1_000_006, 25, "LP reward".into(), now_unix);
    });
    assert_eq!(
        rpc(&ctx, &mut sess2, "invbroker", "DeliverToCorpHangar", &[PyRep::Integer(999_999_999), PyRep::Integer(1)]).await.unwrap_err().code,
        "ItemNotFound"
    );
    rpc(&ctx, &mut sess2, "invbroker", "DeliverToCorpHangar", &[PyRep::Integer(trit_item_id), PyRep::Integer(1)])
        .await
        .expect("DeliverToCorpHangar Tritanium");

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.corp_projects[&pid_mine].current_quantity, 10);
        assert_eq!(s.corp_projects[&pid_npc].current_quantity, 1);
        assert_eq!(s.corp_projects[&pid_lp].current_quantity, 25);
        assert_eq!(s.corp_projects[&pid_deliv].current_quantity, 5);
        assert_eq!(s.items[&trit_item_id].owner_id, corp_id, "DeliverToCorpHangar must transfer 5 Tritanium to corporation hangar");
        assert_eq!(s.characters[&PILOT_2].balance.raw_cents(), 135_000_00);
    });

    // Verify live ESI `GET /v1/corporations/{corp_id}/projects/` reads unified Quasar projects & contributors
    let router = eve_server::esi::EsiRouter::new(std::sync::Arc::clone(&ctx));
    let spec = eve_server::esi::OpenApiSpec::pinned();
    let code_noscope = ctx.db.authorize_pkce("demo", "demo-secret", PILOT_1, "b22-esi", "https://localhost/cb", vec![], "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", "S256", now_unix).unwrap();
    let tok_noscope = ctx.db.exchange_pkce_code(&code_noscope, "b22-esi", "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", now_unix).unwrap().access_token;
    assert_eq!(router.handle_request(&eve_server::esi::EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/projects/")).with_bearer_token(&tok_noscope)).status_code, 403);
    let code_ok = ctx.db.authorize_pkce("demo", "demo-secret", PILOT_1, "b22-esi", "https://localhost/cb", vec!["esi-corporations.read_projects.v1".into()], "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", "S256", now_unix).unwrap();
    let tok_ok = ctx.db.exchange_pkce_code(&code_ok, "b22-esi", "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", now_unix).unwrap().access_token;
    let esi_resp = router.handle_request(&eve_server::esi::EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/projects/")).with_bearer_token(&tok_ok));
    assert_eq!(esi_resp.status_code, 200);
    spec.validate_response("GET", &format!("/v1/corporations/{corp_id}/projects/"), esi_resp.status_code, &esi_resp.headers, &esi_resp.body).expect("OpenAPI projects schema");
    let etag = esi_resp.header("etag").expect("ETag header").to_string();
    assert_eq!(router.handle_request(&eve_server::esi::EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/projects/")).with_bearer_token(&tok_ok).with_header("If-None-Match", &etag)).status_code, 304);
    let esi_json: serde_json::Value = serde_json::from_slice(&esi_resp.body).unwrap();
    let mine_entry = esi_json.as_array().unwrap().iter().find(|p| p["project_id"] == pid_mine).expect("Mine Veldspar in ESI projects");
    assert_eq!(mine_entry["current_quantity"], 10);
    assert_eq!(mine_entry["isk_per_unit"], 10_000.0);
    assert_eq!(mine_entry["escrow_remaining"], 900_000.0);
    assert_eq!(mine_entry["contributors"], serde_json::json!([{ "character_id": PILOT_2, "contributed_quantity": 10 }]));

    // Close project via Quasar and verify remaining escrow (900,000 ISK) refunded
    let bal_before_close = ctx.db.with_snapshot(|s| s.corporations[&corp_id].wallet_divisions[0].balance.raw_cents());
    assert_eq!(q_req(&sess1, "eve_public.goal.api.CloseRequest", mine_id_wire).status_code, eve_server::quasar_gateway::STATUS_OK);
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
    assert!((small_net - 50_000.0).abs() < 1e-3, "Bounty under 100k ISK must be tax-exempt");

    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((sys_id, PILOT_2), 1_000_000.0);
    });
    let (large_net, _, _) = eve_server::services::combat_npc_encounters::flush_npc_bounty_tick(&ctx, sys_id, PILOT_2, 0.0);
    assert!((large_net - 890_000.0).abs() < 1e-3, "11% NPC corp tax must apply above 100k ISK threshold");
    ctx.db.with_snapshot(|s| {
        assert!(
            s.wallet_journal.iter().any(|j| j.character_id == PILOT_2 && j.entry_type == "bounty_prize_corporation_tax" && j.amount.raw_cents() == -110_000_00),
            "Wallet journal must record 11% NPC corp tax deduction"
        );
    });
}
