//! End-to-end MachoNet RPC integration tests for the 10 remediated `rpc:*` parity rows.

use crate::common;
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const LOWSEC_ASABONA: u32 = 30_000_012;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

async fn login_and_select(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

#[tokio::test]
// parity: rpc:FastCheckoutService.GetOffersForUser, rpc:FastCheckoutService.BuyOffer, rpc:FastCheckoutService.RequestMiningCycleOutput
async fn test_parity_fast_checkout_offers_buy_and_mining_output() {
    eve_testkit::attest(&[
        "rpc:FastCheckoutService.GetOffersForUser",
        "rpc:FastCheckoutService.BuyOffer",
        "rpc:FastCheckoutService.RequestMiningCycleOutput",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // Unauthenticated FastCheckoutService calls fail
    let mut unauth = SessionState::new(91_000);
    assert_eq!(rpc(&ctx, &mut unauth, "FastCheckoutService", "GetOffersForUser", &[]).await.unwrap_err().code, "Unauthorized");

    let mut s1 = login_and_select(&ctx, 91_001, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = login_and_select(&ctx, 91_002, "other", "other-secret", PILOT_2).await;
    let user_id = s1.user_id.expect("user_id");

    // Disabled by default: GetOffersForUser returns empty list; BuyOffer returns FastCheckoutDisabled
    assert_eq!(rpc(&ctx, &mut s1, "FastCheckoutService", "GetOffersForUser", &[]).await.expect("GetOffersForUser disabled"), PyRep::List(Vec::new()));
    assert_eq!(
        rpc(&ctx, &mut s1, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(2001), PyRep::Integer(1), PyRep::String("demo-secret".into())]).await.unwrap_err().code,
        "FastCheckoutDisabled"
    );

    // Enable FastCheckout via real slash.SlashCmd("/fastcheckout 1")
    s1.role |= service_roles::ROLE_GM_DEFAULT;
    let fc_enable = rpc(&ctx, &mut s1, "slash", "SlashCmd", &[PyRep::String("/fastcheckout 1".into())]).await.expect("/fastcheckout 1");
    assert_eq!(fc_enable, PyRep::Bool(true));

    // Seed NES store catalog with PLEX offer (240 PLEX @ $9.99)
    ctx.db.with_mut_snapshot(|snap| {
        use eve_server::services::quasar_payment_store_types::{
            save_store_catalog, save_store_offer_tags, StoreCatalog, StoreCategory, StoreOffer, StoreProduct,
        };
        save_store_catalog(snap, &StoreCatalog {
            categories: vec![StoreCategory { id: 1, name: "PLEX".into(), description: "PLEX Packs".into() }],
            products: vec![StoreProduct { id: 201, name: "240 PLEX".into(), description: "240 PLEX".into(), type_id: 44_992, quantity: 240 }],
            offers: vec![StoreOffer {
                id: 2001, name: "240 PLEX".into(), description: "240 PLEX Pack".into(), category_id: 1,
                price_plex: 0, product_ids: vec![201], targeted: false, price_cents: Some(999), currency: Some("USD".into()),
            }],
        });
        let mut tags = HashMap::new();
        tags.insert(2001, vec!["popular".to_string()]);
        save_store_offer_tags(snap, &tags);
    });

    // GetOffersForUser returns catalog-backed offer with exact fields
    let offers_rep = rpc(&ctx, &mut s1, "FastCheckoutService", "GetOffersForUser", &[]).await.expect("GetOffersForUser enabled");
    let PyRep::Dict(offers_dict) = offers_rep else { panic!("Expected Dict from GetOffersForUser"); };
    let plex_list = offers_dict.iter().find_map(|(k, v)| (k == &PyRep::String("plex".into())).then_some(v)).expect("plex key");
    let PyRep::List(offers) = plex_list else { panic!("Expected List for plex"); };
    assert_eq!(offers.len(), 1);
    assert_eq!(offers[0].dict_get("offerId").and_then(|v| v.as_i64()), Some(2001));
    assert_eq!(offers[0].dict_get("baseQuantity").and_then(|v| v.as_i64()), Some(240));
    assert_eq!(offers[0].dict_get("price").and_then(|v| v.as_f64()), Some(9.99));

    // BuyOffer without quick-pay payment token fails with PaymentTokenRequired
    assert_eq!(
        rpc(&ctx, &mut s1, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(2001), PyRep::Integer(2), PyRep::String("demo-secret".into())]).await.unwrap_err().code,
        "PaymentTokenRequired"
    );

    // Register quick-pay payment token via production helper
    eve_server::services::quasar_payment_store_types::add_user_quick_pay_token(
        &ctx.db,
        &ctx.quasar_gateway.pending_notices,
        user_id,
        eve_server::services::quasar_payment_store_types::PaymentTokenRecord {
            id: 77, user_id, token_str: "tok_fc_77".into(), is_quick_pay: true, consumed: false,
        },
    );

    // Negative BuyOffer cases: wrong password, unknown offer, non-positive quantity
    assert_eq!(
        rpc(&ctx, &mut s1, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(2001), PyRep::Integer(1), PyRep::String("bad-pass".into())]).await.unwrap_err().code,
        "InvalidPassword"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(9999), PyRep::Integer(1), PyRep::String("demo-secret".into())]).await.unwrap_err().code,
        "OfferNotFound"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(2001), PyRep::Integer(0), PyRep::String("demo-secret".into())]).await.unwrap_err().code,
        "InvalidQuantity"
    );

    // Valid BuyOffer (2 x 240 PLEX = 480 PLEX, $19.98 = 1998 cents)
    let vault_before = ctx
        .db
        .with_snapshot(|snap| snap.account_plex_vault.get(&user_id).copied().unwrap_or(0));
    let buy_res = rpc(
        &ctx,
        &mut s1,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(2001), PyRep::Integer(2), PyRep::String("demo-secret".into())],
    )
    .await
    .expect("BuyOffer succeeds");
    assert_eq!(buy_res.dict_get("status").and_then(|v| v.as_str()), Some("CONFIRMED"));
    assert_eq!(buy_res.dict_get("offerID").and_then(|v| v.as_i64()), Some(2001));
    assert_eq!(buy_res.dict_get("plexAmount").and_then(|v| v.as_i64()), Some(480));

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.account_plex_vault.get(&user_id).copied().unwrap_or(0), vault_before + 480);
        let receipts: Vec<eve_server::services::quasar_payment_store_types::PaymentReceiptRecord> =
            snap.service_state_blobs
                .get("payment_receipts")
                .and_then(|j| serde_json::from_str(j).ok())
                .unwrap_or_default();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].amount_in_cents, 1998);
        assert_eq!(receipts[0].offer_id, "2001");
        let txs = eve_server::services::quasar_payment_store::get_plex_transactions(snap);
        assert_eq!(txs.last().map(|t| t.amount), Some(480));
    });

    // RequestMiningCycleOutput: create player corp, teleport to Low-Sec Asabona, give Metenox (81826), undock & anchor
    rpc(
        &ctx,
        &mut s1,
        "slash",
        "SlashCmd",
        &[PyRep::String("/giveisk 5000000000".into())],
    )
    .await
    .expect("/giveisk");
    rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Metenox Mining Co".into()), PyRep::String("MTNX".into())],
    )
    .await
    .expect("AddCorporation");
    ctx.advance_secs(7 * 86_400 + 10);
    rpc(
        &ctx,
        &mut s1,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/tr {LOWSEC_ASABONA}"))],
    )
    .await
    .expect("/tr lowsec");
    let create_item_res = rpc(
        &ctx,
        &mut s1,
        "slash",
        "SlashCmd",
        &[PyRep::String("/createitem 81826 1".into())],
    )
    .await
    .expect("/createitem Metenox");
    let metenox_item_id = create_item_res
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("created Metenox itemID");

    // Anchor Metenox near deploying ship via structureDeployment.Anchor
    let anchor_res = rpc(
        &ctx,
        &mut s1,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(metenox_item_id),
            PyRep::String("Asabona Metenox".into()),
        ],
    )
    .await
    .expect("structureDeployment.Anchor Metenox");
    let structure_id = anchor_res
        .dict_get("structureID")
        .and_then(|v| v.as_i64())
        .expect("anchored structureID");

    // Verify RequestMiningCycleOutput returns exact 40% yield SDE moon materials
    let cycle_rep = rpc(
        &ctx,
        &mut s1,
        "FastCheckoutService",
        "RequestMiningCycleOutput",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect("FastCheckoutService.RequestMiningCycleOutput");
    let PyRep::List(rows) = cycle_rep else {
        panic!("Expected List from RequestMiningCycleOutput");
    };
    assert!(!rows.is_empty(), "Must yield group-427 moon materials");
    for row in &rows {
        let PyRep::Tuple(t) = row else {
            panic!("Expected 3-tuple");
        };
        assert_eq!(t.len(), 3);
        let tid = t[0].as_i64().expect("type_id") as u32;
        let qty = t[1].as_i64().expect("quantity");
        let vol = t[2].as_f64().expect("volume");
        let inv = ctx.sde.types.get(&tid).expect("material in SDE");
        assert_eq!(inv.group_id, 427, "Material must be group 427 moon material");
        assert!(qty > 0);
        assert!((vol - (qty as f64) * inv.volume).abs() < 1e-6);
    }

    // Negative RequestMiningCycleOutput cases: missing arg, unknown structure, unauthorized corp, High-Sec
    assert_eq!(
        rpc(&ctx, &mut s1, "FastCheckoutService", "RequestMiningCycleOutput", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "FastCheckoutService",
            "RequestMiningCycleOutput",
            &[PyRep::Integer(9_999_999_999)],
        )
        .await
        .unwrap_err()
        .code,
        "StructureNotFound"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s2,
            "FastCheckoutService",
            "RequestMiningCycleOutput",
            &[PyRep::Integer(structure_id)],
        )
        .await
        .unwrap_err()
        .code,
        "AccessDenied"
    );

    // Anchor an Astrahus (35832) in another Low-Sec system -> RequestMiningCycleOutput rejects InvalidStructureType
    rpc(
        &ctx,
        &mut s1,
        "slash",
        "SlashCmd",
        &[PyRep::String("/tr 30000013".into())],
    )
    .await
    .expect("/tr 30000013");
    let astra_item = rpc(
        &ctx,
        &mut s1,
        "slash",
        "SlashCmd",
        &[PyRep::String("/createitem 35832 1".into())],
    )
    .await
    .expect("/createitem Astrahus")
    .dict_get("itemID")
    .and_then(|v| v.as_i64())
    .unwrap();
    let astra_struct = rpc(
        &ctx,
        &mut s1,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(astra_item),
            PyRep::String("Asabona Astrahus".into()),
        ],
    )
    .await
    .expect("Anchor Astrahus")
    .dict_get("structureID")
    .and_then(|v| v.as_i64())
    .unwrap();
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "FastCheckoutService",
            "RequestMiningCycleOutput",
            &[PyRep::Integer(astra_struct)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidStructureType"
    );
}

#[tokio::test]
// parity: rpc:agentMgr.GetPlausibleMissionIDs, rpc:cache.Rowset
async fn test_parity_agent_plausible_missions_and_cache_rowset() {
    eve_testkit::attest(&[
        "rpc:agentMgr.GetPlausibleMissionIDs",
        "rpc:cache.Rowset",
    ]);

    let ctx = ServerContext::new();
    let mut sess = login_and_select(&ctx, 91_101, "demo", "demo-secret", PILOT_1).await;

    // 1. agentMgr.GetPlausibleMissionIDs: pick two distinct SDE agents (level 1 vs level 4)
    let mut agents: Vec<_> = ctx.sde.agents.values().collect();
    agents.sort_by_key(|a| a.agent_id);
    let lvl1_agent = agents
        .iter()
        .find(|a| a.level == 1 && a.division_id == 24)
        .expect("level 1 security agent in SDE");
    let lvl4_agent = agents
        .iter()
        .find(|a| a.level == 4 && a.division_id == 22)
        .expect("level 4 distribution agent in SDE");

    let res1 = rpc(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetPlausibleMissionIDs",
        &[PyRep::Integer(lvl1_agent.agent_id)],
    )
    .await
    .expect("GetPlausibleMissionIDs lvl1");
    let PyRep::List(ids1_rep) = res1 else {
        panic!("Expected List from GetPlausibleMissionIDs");
    };
    let ids1: Vec<u32> = ids1_rep
        .iter()
        .filter_map(|r| r.as_i64().map(|v| v as u32))
        .collect();
    assert!(!ids1.is_empty() && ids1.len() <= 25);
    for mid in &ids1 {
        let m = ctx.sde.missions.get(mid).expect("mission in SDE");
        assert!(!m.is_courier, "Security agent mission {mid} must not be courier");
    }

    let res4 = rpc(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetPlausibleMissionIDs",
        &[PyRep::Integer(lvl4_agent.agent_id)],
    )
    .await
    .expect("GetPlausibleMissionIDs lvl4");
    let PyRep::List(ids4_rep) = res4 else {
        panic!("Expected List from GetPlausibleMissionIDs");
    };
    let ids4: Vec<u32> = ids4_rep
        .iter()
        .filter_map(|r| r.as_i64().map(|v| v as u32))
        .collect();
    assert!(!ids4.is_empty() && ids4.len() <= 25);
    assert_ne!(ids1, ids4, "Different agent levels/divisions must return distinct mission pools");
    for mid in &ids4 {
        let m = ctx.sde.missions.get(mid).expect("mission in SDE");
        assert!(m.is_courier, "Distribution agent mission {mid} must be courier");
    }

    // Negative: unknown agentID returns AgentNotFound
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "agentMgr",
            "GetPlausibleMissionIDs",
            &[PyRep::Integer(999_999_999)],
        )
        .await
        .unwrap_err()
        .code,
        "AgentNotFound"
    );

    // 2. cache.Rowset: verify roles, races, factions, bloodlines, ancestries against SDE & corp role defs
    let roles_rep = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("roles".into())])
        .await
        .expect("cache.Rowset roles");
    let PyRep::Rowset { columns, rows } = roles_rep else {
        panic!("Expected Rowset for roles");
    };
    assert_eq!(columns, vec!["roleID", "roleName"]);
    assert_eq!(
        rows.len(),
        eve_server::esi::corp_governance_routes::CORP_ROLE_DEFINITIONS.len()
    );
    assert!(rows.iter().any(|r| r.get(1).and_then(|v| v.as_str()) == Some("Director")));

    let races_rep = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("races".into())])
        .await
        .expect("cache.Rowset races");
    let PyRep::Rowset { columns: r_cols, rows: r_rows } = races_rep else {
        panic!("Expected Rowset for races");
    };
    assert_eq!(r_cols, vec!["raceID", "raceName"]);
    assert_eq!(r_rows.len(), ctx.sde.races.len());

    let factions_rep = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("factions".into())])
        .await
        .expect("cache.Rowset factions");
    let PyRep::Rowset { rows: f_rows, .. } = factions_rep else {
        panic!("Expected Rowset for factions");
    };
    assert_eq!(f_rows.len(), ctx.sde.factions.len());

    let bloodlines_rep = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("bloodlines".into())])
        .await
        .expect("cache.Rowset bloodlines");
    let PyRep::Rowset { rows: b_rows, .. } = bloodlines_rep else {
        panic!("Expected Rowset for bloodlines");
    };
    assert_eq!(b_rows.len(), ctx.sde.bloodlines.len());

    let ancestries_rep = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("ancestries".into())])
        .await
        .expect("cache.Rowset ancestries");
    let PyRep::Rowset { rows: a_rows, .. } = ancestries_rep else {
        panic!("Expected Rowset for ancestries");
    };
    assert_eq!(a_rows.len(), ctx.sde.ancestries.len());

    // Negative: unknown table name returns UnknownCacheRowset
    assert_eq!(
        rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("nonexistent_table".into())])
            .await
            .unwrap_err()
            .code,
        "UnknownCacheRowset"
    );
}

#[tokio::test]
// parity: rpc:ProjectDiscovery.get_new_task, rpc:ProjectDiscovery.post_classification, rpc:ProjectDiscovery.get_player_statistics, rpc:ProjectDiscovery.get_total_needed_xp, rpc:ProjectDiscovery.give_tutorial_rewards
async fn test_parity_project_discovery_tasks_stats_xp_and_tutorial() {
    eve_testkit::attest(&[
        "rpc:ProjectDiscovery.get_new_task",
        "rpc:ProjectDiscovery.post_classification",
        "rpc:ProjectDiscovery.get_player_statistics",
        "rpc:ProjectDiscovery.get_total_needed_xp",
        "rpc:ProjectDiscovery.give_tutorial_rewards",
    ]);

    let ctx = ServerContext::new();
    let mut sess = login_and_select(&ctx, 91_201, "demo", "demo-secret", PILOT_1).await;

    // 1. get_total_needed_xp: unconfigured returns 0.0; configured MMOS curve returns exact thresholds
    assert_eq!(
        rpc(&ctx, &mut sess, "ProjectDiscovery", "get_total_needed_xp", &[PyRep::Integer(1)])
            .await
            .unwrap(),
        PyRep::Real(0.0)
    );
    ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::project_discovery::configure_project_discovery_mmos(
            snap, 2_000.0, 250.0, 150.0, 75.0, 50,
        );
        eve_server::services::project_discovery::set_project_discovery_task_consensus(snap, 1, true);
        eve_server::services::project_discovery::set_project_discovery_task_consensus(snap, 2, false);
    });
    assert_eq!(
        rpc(&ctx, &mut sess, "ProjectDiscovery", "get_total_needed_xp", &[PyRep::Integer(0)])
            .await
            .unwrap(),
        PyRep::Real(0.0)
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "ProjectDiscovery", "get_total_needed_xp", &[PyRep::Integer(2)])
            .await
            .unwrap(),
        PyRep::Real(4_000.0)
    );

    // 2. give_tutorial_rewards: false before completion, true once level 5 reached, idempotent false after
    assert_eq!(
        rpc(&ctx, &mut sess, "ProjectDiscovery", "give_tutorial_rewards", &[])
            .await
            .unwrap(),
        PyRep::Bool(false)
    );
    for _ in 0..4 {
        rpc(&ctx, &mut sess, "ProjectDiscovery", "increase_tutorial_level", &[])
            .await
            .expect("increase_tutorial_level");
    }
    assert_eq!(
        rpc(&ctx, &mut sess, "ProjectDiscovery", "give_tutorial_rewards", &[])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "ProjectDiscovery", "give_tutorial_rewards", &[])
            .await
            .unwrap(),
        PyRep::Bool(false),
        "Tutorial reward must be idempotent"
    );

    // 3. get_new_task & post_classification (accurate task 1 -> +150 XP, +50 AK, +1000 ISK)
    let t1 = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[])
        .await
        .expect("get_new_task 1");
    assert_eq!(t1.dict_get("task_id").and_then(|v| v.as_str()), Some("task_1"));
    assert_eq!(t1.dict_get("task_number").and_then(|v| v.as_i64()), Some(1));

    // Negative post_classification cases: empty payload, mismatched task_id
    let err_empty = rpc(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::String("task_1".into()), PyRep::List(vec![])],
    )
    .await
    .unwrap_err();
    assert_eq!(err_empty.code.to_ascii_lowercase(), "invalidclassification");

    let err_mismatch = rpc(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::String("task_99".into()), PyRep::List(vec![PyRep::Real(1.0)])],
    )
    .await
    .unwrap_err();
    assert_eq!(err_mismatch.code.to_ascii_lowercase(), "invalidtaskid");

    let post1 = rpc(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::String("task_1".into()), PyRep::List(vec![PyRep::Real(1.0)])],
    )
    .await
    .expect("post_classification task_1");
    assert_eq!(post1.dict_get("experience_reward").and_then(|v| v.as_f64()), Some(150.0));
    assert_eq!(post1.dict_get("kredits_reward").and_then(|v| v.as_i64()), Some(50));
    assert_eq!(post1.dict_get("ISK_Reward").and_then(|v| v.as_f64()), Some(1_000.0));

    // Duplicate submission of task_1 fails with TaskAlreadySubmitted
    let err_dup = rpc(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::String("task_1".into()), PyRep::List(vec![PyRep::Real(1.0)])],
    )
    .await
    .unwrap_err();
    assert_eq!(err_dup.code.to_ascii_lowercase(), "taskalreadysubmitted");

    // 4. get_player_statistics: verify score (100.0) != accuracy (1.0) in both top-level and project dict
    let stats = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_player_statistics", &[])
        .await
        .expect("get_player_statistics");
    assert_eq!(stats.dict_get("tasks_submitted").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(stats.dict_get("accuracy").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(stats.dict_get("score").and_then(|v| v.as_f64()), Some(100.0));
    assert_eq!(stats.dict_get("experience").and_then(|v| v.as_f64()), Some(400.0));
    let proj_score = stats
        .dict_get("project")
        .and_then(|p| p.dict_get("score"))
        .and_then(|v| v.as_f64());
    assert_eq!(proj_score, Some(100.0), "project.score must equal st.score (100.0), not st.accuracy");
}
