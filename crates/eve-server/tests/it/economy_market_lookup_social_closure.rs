//! Live RPC integration tests for standings, New Eden Store, search, logon milestones,
//! browser lockdown policy, ship SKIN application, paperdoll apparel, and Neocom badges.
#![forbid(unsafe_code)]
use eve_db::{AllianceRecord, CorporationRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;
fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| match k {
            PyRep::String(s) if s == key => Some(v),
            _ => None,
        }),
        PyRep::KeyVal(entries) => entries.iter().find_map(|(k, v)| (k == key).then_some(v)),
        PyRep::Object { args, .. } => dict_get(args, key),
        _ => None,
    }
}
fn extract_rowset_lines(rep: &PyRep) -> Vec<Vec<PyRep>> {
    match rep {
        PyRep::Rowset { rows, .. } => rows.clone(),
        other => panic!("expected PyRep::Rowset, got {other:?}"),
    }
}
#[tokio::test]
async fn test_npc_npc_standings_rowset_and_auth() {
    // parity: rpc:standingMgr.GetNPCNPCStandings, feat:standings.npc-npc-standings
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let empty_kw = HashMap::new();
    let mut unauth = SessionState::new(4101);
    let err = ctx
        .dispatch_rpc(
            &mut unauth,
            "standingMgr",
            None,
            "GetNPCNPCStandings",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect_err("unauthenticated GetNPCNPCStandings must fail");
    assert!(
        err.code == "Unauthorized" || err.code == "SessionNotFound" || err.code == "AuthRequired",
        "unexpected auth error: {err:?}"
    );
    let mut sess = SessionState::new(4102);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(98_000_001);
    let rep = ctx
        .dispatch_rpc(
            &mut sess,
            "standingMgr",
            None,
            "GetNPCNPCStandings",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("GetNPCNPCStandings");
    let rows = extract_rowset_lines(&rep);
    assert!(
        rows.len() >= 50,
        "expected >= 50 static NPC-NPC standing pairs, got {}",
        rows.len()
    );
    let amarr_to_caldari = rows.iter().find(|r| {
        matches!(
            (r.first(), r.get(1)),
            (Some(PyRep::Integer(500_001)), Some(PyRep::Integer(500_003)))
        )
    });
    assert!(
        matches!(amarr_to_caldari.and_then(|r| r.get(2)), Some(PyRep::Real(v)) if (*v - 5.0).abs() < 1e-6),
        "expected Amarr -> Caldari standing +5.0, got {amarr_to_caldari:?}"
    );
}
#[tokio::test]
async fn test_nes_store_plex_purchase_delivery_and_negative_cases() {
    // parity: feat:wallet.nes-store
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let empty_kw = HashMap::new();
    let mut unauth = SessionState::new(4103);
    assert!(
        ctx.dispatch_rpc(
            &mut unauth,
            "storeManager",
            None,
            "buy_offer",
            &[PyRep::Integer(1)],
            &empty_kw,
            None,
        )
        .await
        .is_err(),
        "unauthenticated buy_offer must fail"
    );
    let mut sess = SessionState::new(4104);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(98_000_001);
    sess.station_id = Some(60_003_760);
    ctx.db.with_mut_snapshot(|snap| {
        use eve_server::services::quasar_payment_store_types::{
            save_store_catalog, StoreCatalog, StoreCategory, StoreOffer, StoreProduct,
        };
        save_store_catalog(
            snap,
            &StoreCatalog {
                categories: vec![StoreCategory {
                    id: 1,
                    name: "Apparel".into(),
                    description: "Pilot apparel".into(),
                }],
                products: vec![StoreProduct {
                    id: 10,
                    name: "Tritanium Crate".into(),
                    description: "Test NES item".into(),
                    type_id: 34,
                    quantity: 100,
                }],
                offers: vec![StoreOffer {
                    id: 100,
                    name: "Tritanium Bundle".into(),
                    description: "100x Tritanium".into(),
                    category_id: 1,
                    currency: Some("PLX".into()),
                    price_plex: 50,
                    price_cents: None,
                    product_ids: vec![10],
                    targeted: false,
                }],
            },
        );
    });
    let PyRep::List(cats) = ctx
        .dispatch_rpc(&mut sess, "storeManager", None, "get_categories", &[], &empty_kw, None)
        .await
        .expect("get_categories")
    else {
        panic!("expected categories list");
    };
    assert!(!cats.is_empty(), "expected non-empty NES categories");
    let PyRep::List(offers) = ctx
        .dispatch_rpc(&mut sess, "storeManager", None, "get_offers", &[], &empty_kw, None)
        .await
        .expect("get_offers")
    else {
        panic!("expected offers list");
    };
    assert!(!offers.is_empty(), "expected non-empty NES offers");
    let first_offer = &offers[0];
    let offer_id = dict_get(first_offer, "offerID")
        .and_then(|v| v.as_i64())
        .expect("offerID");
    let price_plex = dict_get(first_offer, "plexPrice")
        .and_then(|v| v.as_i64())
        .expect("plexPrice");
    // AUR currency must be rejected with AurumCurrencyRemoved
    let aur_err = ctx
        .dispatch_rpc(
            &mut sess,
            "storeManager",
            None,
            "buy_offer",
            &[PyRep::Integer(offer_id), PyRep::String("AUR".into())],
            &empty_kw,
            None,
        )
        .await
        .expect_err("AUR currency must be rejected");
    assert_eq!(aur_err.code, "AurumCurrencyRemoved");
    // Insufficient PLEX when vault is drained
    ctx.db.with_mut_snapshot(|snap| {
        let cur = snap.plex_vault_balance(90_000_001);
        if cur > 0 {
            snap.adjust_plex_vault(90_000_001, -cur);
        }
    });
    let can_buy_before = ctx
        .dispatch_rpc(
            &mut sess,
            "storeManager",
            None,
            "can_purchase_offer",
            &[PyRep::Integer(offer_id)],
            &empty_kw,
            None,
        )
        .await
        .expect("can_purchase_offer");
    assert_eq!(can_buy_before, PyRep::Bool(false));
    let insuf_err = ctx
        .dispatch_rpc(
            &mut sess,
            "storeManager",
            None,
            "buy_offer",
            &[PyRep::Integer(offer_id)],
            &empty_kw,
            None,
        )
        .await
        .expect_err("insufficient PLEX must fail");
    assert_eq!(insuf_err.message.contains("PLEX"), true);
    // Fund PLEX vault and purchase
    let start_plex = price_plex + 250;
    ctx.db.with_mut_snapshot(|snap| {
        snap.adjust_plex_vault(90_000_001, start_plex);
    });
    let can_buy_after = ctx
        .dispatch_rpc(
            &mut sess,
            "storeManager",
            None,
            "can_purchase_offer",
            &[PyRep::Integer(offer_id)],
            &empty_kw,
            None,
        )
        .await
        .expect("can_purchase_offer funded");
    assert_eq!(can_buy_after, PyRep::Bool(true));
    let items_before = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.owner_id == 90_000_001 && i.location_id == 60_003_760)
            .count()
    });
    ctx.dispatch_rpc(
        &mut sess,
        "storeManager",
        None,
        "buy_offer",
        &[PyRep::Integer(offer_id), PyRep::String("PLX".into()), PyRep::Integer(1)],
        &empty_kw,
        None,
    )
    .await
    .expect("buy_offer");
    let (rem_plex, items_after) = ctx.db.with_snapshot(|snap| {
        let bal = snap.plex_vault_balance(90_000_001);
        let cnt = snap
            .items
            .values()
            .filter(|i| i.owner_id == 90_000_001 && i.location_id == 60_003_760)
            .count();
        (bal, cnt)
    });
    assert_eq!(rem_plex, 250, "PLEX vault must be debited by offer price");
    assert!(
        items_after > items_before,
        "purchased NES product must be delivered to character hangar"
    );
}
#[tokio::test]
async fn test_people_and_places_search_query_and_quick_query() {
    // parity: feat:ui.search
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let empty_kw = HashMap::new();
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            98_000_222,
            CorporationRecord {
                corporation_id: 98_000_222,
                name: "Starlight Cartography".into(),
                ticker: "SLCT".into(),
                ceo_id: 90_000_001,
                ..Default::default()
            },
        );
        snap.alliances.insert(
            99_000_222,
            AllianceRecord {
                alliance_id: 99_000_222,
                name: "Nebula Accord".into(),
                short_name: "NEBA".into(),
                executor_corp_id: 98_000_222,
                ..Default::default()
            },
        );
    });
    let mut sess = SessionState::new(4105);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    // Negative: empty query and banned word
    let short_err = ctx
        .dispatch_rpc(
            &mut sess,
            "search",
            None,
            "Query",
            &[PyRep::String("   ".into())],
            &empty_kw,
            None,
        )
        .await
        .expect_err("empty search string must fail");
    assert_eq!(short_err.code, "SearchStringTooShort");
    let banned_err = ctx
        .dispatch_rpc(
            &mut sess,
            "search",
            None,
            "Query",
            &[PyRep::String("exploit".into())],
            &empty_kw,
            None,
        )
        .await
        .expect_err("banned word must fail");
    assert_eq!(banned_err.code, "UserError");
    // Partial match for corp & alliance
    let rep = ctx
        .dispatch_rpc(
            &mut sess,
            "search",
            None,
            "Query",
            &[
                PyRep::String("Starlight".into()),
                PyRep::List(vec![PyRep::Integer(3)]),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("search.Query corp");
    let PyRep::Dict(entries) = rep else {
        panic!("expected search result dict");
    };
    let corp_hits = entries
        .iter()
        .find_map(|(k, v)| (k.as_i64() == Some(3)).then_some(v))
        .expect("group 3 key");
    let PyRep::List(ids) = corp_hits else {
        panic!("expected corp id list");
    };
    assert!(ids.contains(&PyRep::Integer(98_000_222)));
    // Exact match miss vs hit via QuickQuery
    let PyRep::List(quick_miss) = ctx
        .dispatch_rpc(
            &mut sess,
            "search",
            None,
            "QuickQuery",
            &[
                PyRep::String("Nebula".into()),
                PyRep::List(vec![PyRep::Integer(4)]),
                PyRep::Bool(false),
                PyRep::Bool(false),
                PyRep::Bool(true), // exact = true
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("QuickQuery exact miss")
    else {
        panic!("expected QuickQuery list");
    };
    assert!(!quick_miss.contains(&PyRep::Integer(99_000_222)));
    let PyRep::List(quick_hit) = ctx
        .dispatch_rpc(
            &mut sess,
            "search",
            None,
            "QuickQuery",
            &[
                PyRep::String("NEBA".into()),
                PyRep::List(vec![PyRep::Integer(4)]),
                PyRep::Bool(false),
                PyRep::Bool(true), // onlyAltName = true
                PyRep::Bool(true), // exact = true
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("QuickQuery exact ticker hit")
    else {
        panic!("expected QuickQuery list");
    };
    assert!(quick_hit.contains(&PyRep::Integer(99_000_222)));
}
