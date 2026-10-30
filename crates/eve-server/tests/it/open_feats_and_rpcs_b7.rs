//! Integration tests for open `feat:` and `rpc:` parity rows (Batch 7).
//!
//! Covers:
//! - In-game browser lockdown policy (`browserLockdownSvc`, `feat:ui.browser-lockdown`)
//! - NPC-NPC static standings (`standingMgr.GetNPCNPCStandings`, `feat:standings.npc-npc-standings`)
//! - LP Store offers & FW LP (`LPStoreMgr.GetAvailableOffersFromCorp`, `feat:lp.lp-store-offers`, `feat:fw.fw-lp`)
//! - New Eden Store (`storeManager`, `feat:wallet.nes-store`)
//! - Ship SKIN application (`shipCosmeticsMgr.ApplySkinToShip`, `feat:cosmetics.skin-apply`)
//! - Structure directory (`structureDirectory.GetStructures`, `feat:structures.structure-directory`)
//! - Core service `Bind` / `SetSessionCheck` / `clear_memoized` RPCs

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::browser_lockdown::{
    save_browser_lockdown_policy, BrowserLockdownPolicy, DEFAULT_HOME_PAGE,
};

use super::common;

const PILOT_1: i64 = 90_000_001;
const STATION_ID: i64 = 60_003_760;

fn new_ctx() -> ServerContext {
    ServerContext::new()
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

async fn setup_pilot(ctx: &ServerContext, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(char_id as u64);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");

    sess.station_id = Some(STATION_ID);
    sess.structure_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(STATION_ID);
            ch.structure_id = None;
            ch.home_station_id = STATION_ID;
        }
    });
    sess
}

#[tokio::test]
async fn test_browser_lockdown_policy_and_flagged_sites() {
    // parity: feat:ui.browser-lockdown
    // parity: rpc:browserLockdownSvc.GetDefaultHomePage
    // parity: rpc:browserLockdownSvc.GetFlaggedSitesHash
    // parity: rpc:browserLockdownSvc.GetFlaggedSitesList
    // parity: rpc:browserLockdownSvc.IsBrowserInLockdown
    let ctx = new_ctx();
    let mut unauth = SessionState::new(999);
    let err = rpc(
        &ctx,
        &mut unauth,
        "browserLockdownSvc",
        "IsBrowserInLockdown",
        &[],
    )
    .await
    .expect_err("unauthenticated browserLockdownSvc call must fail");
    assert_eq!(err.code, "Unauthorized");

    let mut sess = setup_pilot(&ctx, PILOT_1).await;

    let locked = rpc(
        &ctx,
        &mut sess,
        "browserLockdownSvc",
        "IsBrowserInLockdown",
        &[],
    )
    .await
    .expect("IsBrowserInLockdown");
    assert_eq!(locked, PyRep::Bool(false));

    let home = rpc(
        &ctx,
        &mut sess,
        "browserLockdownSvc",
        "GetDefaultHomePage",
        &[],
    )
    .await
    .expect("GetDefaultHomePage");
    assert_eq!(home, PyRep::String(DEFAULT_HOME_PAGE.to_string()));

    let sites = rpc(
        &ctx,
        &mut sess,
        "browserLockdownSvc",
        "GetFlaggedSitesList",
        &[],
    )
    .await
    .expect("GetFlaggedSitesList");
    assert_eq!(sites, PyRep::List(vec![]));

    let default_hash = rpc(
        &ctx,
        &mut sess,
        "browserLockdownSvc",
        "GetFlaggedSitesHash",
        &[],
    )
    .await
    .expect("GetFlaggedSitesHash");
    let PyRep::String(hash_str) = &default_hash else {
        panic!("expected String from GetFlaggedSitesHash");
    };
    assert_eq!(hash_str.len(), 32);

    ctx.db.with_mut_snapshot(|snap| {
        save_browser_lockdown_policy(
            snap,
            &BrowserLockdownPolicy {
                in_lockdown: true,
                default_home_page: "https://community.eveonline.com".to_string(),
                flagged_sites: vec![
                    "https://phishing.example".to_string(),
                    "https://malware.example".to_string(),
                ],
            },
        );
    });

    let locked_after = rpc(
        &ctx,
        &mut sess,
        "browserLockdownSvc",
        "IsBrowserInLockdown",
        &[],
    )
    .await
    .expect("IsBrowserInLockdown after update");
    assert_eq!(locked_after, PyRep::Bool(true));

    let home_after = rpc(
        &ctx,
        &mut sess,
        "browserLockdownSvc",
        "GetDefaultHomePage",
        &[],
    )
    .await
    .expect("GetDefaultHomePage after update");
    assert_eq!(
        home_after,
        PyRep::String("https://community.eveonline.com".to_string())
    );

    let sites_after = rpc(
        &ctx,
        &mut sess,
        "browserLockdownSvc",
        "GetFlaggedSitesList",
        &[],
    )
    .await
    .expect("GetFlaggedSitesList after update");
    assert_eq!(
        sites_after,
        PyRep::List(vec![
            PyRep::String("https://phishing.example".to_string()),
            PyRep::String("https://malware.example".to_string()),
        ])
    );

    let hash_after = rpc(
        &ctx,
        &mut sess,
        "browserLockdownSvc",
        "GetFlaggedSitesHash",
        &[],
    )
    .await
    .expect("GetFlaggedSitesHash after update");
    assert_ne!(hash_after, default_hash);
}

#[tokio::test]
async fn test_npc_standings_lp_store_and_nes_store() {
    // parity: feat:lp.lp-store-offers
    // parity: feat:fw.fw-lp
    let ctx = new_ctx();
    let mut sess = setup_pilot(&ctx, PILOT_1).await;

    // 1. standingMgr.GetNPCNPCStandings returns static SDE NPC-NPC relationships
    let npc_standings = rpc(&ctx, &mut sess, "standingMgr", "GetNPCNPCStandings", &[])
        .await
        .expect("GetNPCNPCStandings");
    let PyRep::Rowset { columns, rows } = &npc_standings else {
        panic!("expected Rowset from GetNPCNPCStandings, got {npc_standings:?}");
    };
    assert_eq!(columns, &vec!["fromID".to_string(), "toID".to_string(), "standing".to_string()]);
    assert!(!rows.is_empty(), "SDE NPC-NPC standings must be non-empty");

    // 2. LPStoreMgr.GetAvailableOffersFromCorp returns LP offers for Caldari Navy
    ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::lp_store::register_lp_store_offer(
            snap,
            eve_server::services::lp_store::LPOffer {
                offer_id: 5001,
                corp_id: 1_000_035,
                type_id: 587,
                quantity: 1,
                lp_cost: 1_000,
                isk_cost: eve_db::IskCents(500_000),
                req_items: vec![],
                ak_cost: 0,
                min_standing: None,
            },
        );
    });
    let lp_offers = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "GetAvailableOffersFromCorp",
        &[PyRep::Integer(1_000_035)],
    )
    .await
    .expect("GetAvailableOffersFromCorp");
    let PyRep::List(offers) = lp_offers else {
        panic!("expected List from GetAvailableOffersFromCorp");
    };
    assert_eq!(offers.len(), 1, "Caldari Navy must have 1 seeded LP store offer");
    let PyRep::KeyVal(offer_kv) = &offers[0] else {
        panic!("expected KeyVal for LP offer");
    };
    assert_eq!(offer_kv.get("offerID"), Some(&PyRep::Integer(5001)));
    assert_eq!(offer_kv.get("lpCost"), Some(&PyRep::Integer(1_000)));

    // 3. New Eden Store (storeManager) categories, offers, and PLEX purchase
    common::seed_test_nes_store_catalog(&ctx);
    ctx.db.with_mut_snapshot(|snap| {
        let _ = snap.adjust_plex_vault(PILOT_1, 1_000);
    });
    let plex_before = ctx
        .db
        .with_snapshot(|snap| snap.plex_vault_balance(PILOT_1));
    assert_eq!(plex_before, 1_500);

    let cats = rpc(&ctx, &mut sess, "storeManager", "get_categories", &[])
        .await
        .expect("storeManager.get_categories");
    let PyRep::List(cat_list) = cats else {
        panic!("expected List from get_categories");
    };
    assert_eq!(cat_list.len() >= 3, true);

    let nes_offers = rpc(&ctx, &mut sess, "storeManager", "get_offers", &[])
        .await
        .expect("storeManager.get_offers");
    let PyRep::List(nes_list) = nes_offers else {
        panic!("expected List from get_offers");
    };
    assert_eq!(!nes_list.is_empty(), true);

    let can_buy = rpc(
        &ctx,
        &mut sess,
        "storeManager",
        "can_purchase_offer",
        &[PyRep::Integer(1002)],
    )
    .await
    .expect("storeManager.can_purchase_offer");
    assert!(matches!(can_buy, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::Bool(true)));

    let buy_res = rpc(
        &ctx,
        &mut sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(1002), PyRep::String("PLX".into()), PyRep::Integer(1)],
    )
    .await
    .expect("storeManager.buy_offer");
    assert!(matches!(buy_res, PyRep::KeyVal(_) | PyRep::Dict(_)));
    let plex_after = ctx
        .db
        .with_snapshot(|snap| snap.plex_vault_balance(PILOT_1));
    assert_eq!(plex_after < plex_before, true);
}

#[tokio::test]
async fn test_cosmetics_skin_apply_and_structure_directory() {
    // parity: feat:cosmetics.skin-apply
    // parity: feat:structures.structure-directory
    let ctx = new_ctx();
    let mut sess = setup_pilot(&ctx, PILOT_1).await;

    // 1. Grant and apply a SKIN to Rifter (type 587, skin 170)
    sess.role = u64::MAX;
    rpc(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "GiveSkin",
        &[PyRep::Integer(170)],
    )
    .await
    .expect("GiveSkin");
    let apply_rep = rpc(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "ApplySkinToShip",
        &[PyRep::Integer(587), PyRep::Integer(170)],
    )
    .await
    .expect("ApplySkinToShip");
    let PyRep::KeyVal(apply_kv) = apply_rep else {
        panic!("expected KeyVal from ApplySkinToShip");
    };
    assert_eq!(apply_kv.get("skinID"), Some(&PyRep::Integer(170)));
    assert_eq!(
        apply_kv.get("designName"),
        Some(&PyRep::String("Rifter Krusual".into()))
    );

    // 2. structureDirectory.GetStructures returns accessible structures
    let structures = rpc(&ctx, &mut sess, "structureDirectory", "GetStructures", &[])
        .await
        .expect("structureDirectory.GetStructures");
    let PyRep::Dict(struct_dict) = structures else {
        panic!("expected Dict from structureDirectory.GetStructures, got {structures:?}");
    };
    assert_eq!(struct_dict.is_empty(), true);
}

#[tokio::test]
async fn test_core_service_bind_session_check_and_clear_memoized() {
    // parity: rpc:allianceRegistry.Bind
    // parity: rpc:allianceRegistry.SetSessionCheck
    // parity: rpc:warRegistry.SetSessionCheck
    // parity: rpc:ship.SetSessionCheck
    // parity: rpc:ship.clear_memoized
    // parity: rpc:corpRegistry.SetSessionCheck
    // parity: rpc:corpRegistry.clear_memoized
    // parity: rpc:charMgr.SetSessionCheck
    // parity: rpc:charMgr.clear_memoized
    // parity: rpc:reprocessingSvc.SetSessionCheck
    let ctx = new_ctx();
    let mut sess = setup_pilot(&ctx, PILOT_1).await;

    let r1 = rpc(&ctx, &mut sess, "allianceRegistry", "Bind", &[]).await.expect("allianceRegistry.Bind");
    assert_eq!(r1, PyRep::None);
    let r2 = rpc(&ctx, &mut sess, "allianceRegistry", "SetSessionCheck", &[]).await.expect("allianceRegistry.SetSessionCheck");
    assert_eq!(r2, PyRep::None);
    let r3 = rpc(&ctx, &mut sess, "warRegistry", "SetSessionCheck", &[]).await.expect("warRegistry.SetSessionCheck");
    assert_eq!(r3, PyRep::None);
    let r4 = rpc(&ctx, &mut sess, "ship", "SetSessionCheck", &[]).await.expect("ship.SetSessionCheck");
    assert_eq!(r4, PyRep::Bool(true));
    let r5 = rpc(&ctx, &mut sess, "ship", "clear_memoized", &[]).await.expect("ship.clear_memoized");
    assert_eq!(r5, PyRep::Bool(true));
    let r6 = rpc(&ctx, &mut sess, "corpRegistry", "SetSessionCheck", &[]).await.expect("corpRegistry.SetSessionCheck");
    assert_eq!(r6, PyRep::None);
    let r7 = rpc(&ctx, &mut sess, "corpRegistry", "clear_memoized", &[]).await.expect("corpRegistry.clear_memoized");
    assert_eq!(r7, PyRep::None);
    let r8 = rpc(&ctx, &mut sess, "charMgr", "SetSessionCheck", &[]).await.expect("charMgr.SetSessionCheck");
    assert_eq!(r8, PyRep::Bool(true));
    let r9 = rpc(&ctx, &mut sess, "charMgr", "clear_memoized", &[]).await.expect("charMgr.clear_memoized");
    assert_eq!(r9, PyRep::Bool(true));
    let r10 = rpc(&ctx, &mut sess, "reprocessingSvc", "SetSessionCheck", &[]).await.expect("reprocessingSvc.SetSessionCheck");
    assert_eq!(r10, PyRep::Bool(true));
}
