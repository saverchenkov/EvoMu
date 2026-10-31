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
    // parity: feat:ui.search, rpc:search.Query, rpc:search.QuickQuery
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let empty_kw = HashMap::new();
    let allowed_struct_id = 1_030_000_111_001i64;
    let denied_struct_id = 1_030_000_111_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = 98_000_001;
        }
        snap.corporations.insert(
            98_000_001,
            CorporationRecord {
                corporation_id: 98_000_001,
                name: "Vanguard Logistics".into(),
                ticker: "VGLG".into(),
                ceo_id: 90_000_001,
                ..Default::default()
            },
        );
        snap.corporations.insert(
            98_000_222,
            CorporationRecord {
                corporation_id: 98_000_222,
                name: "Starlight Cartography".into(),
                ticker: "SLCT".into(),
                ceo_id: 90_000_002,
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
        snap.structures.insert(
            allowed_struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: allowed_struct_id,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_142,
                name: "Jita - Starlight Hub".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            denied_struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: denied_struct_id,
                type_id: 35832,
                owner_corp_id: 98_000_222,
                solar_system_id: 30_000_142,
                name: "Jita - Starlight BlackSite".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.structure_docking_acl_denied
            .entry(denied_struct_id)
            .or_default()
            .insert(90_000_001);
    });
    // Negative: unauthenticated / character-unselected session rejected
    let mut no_char = SessionState::new(4104);
    no_char.user_id = Some(1);
    assert!(
        ctx.dispatch_rpc(&mut no_char, "search", None, "Query", &[PyRep::String("Jita".into())], &empty_kw, None)
            .await
            .is_err()
    );
    assert!(
        ctx.dispatch_rpc(&mut no_char, "search", None, "QuickQuery", &[PyRep::String("Jita".into())], &empty_kw, None)
            .await
            .is_err()
    );

    let mut sess = SessionState::new(4105);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    let char_name = ctx.db.with_snapshot(|s| s.characters[&90_000_001].name.clone());
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

    // Helper for grouped search.Query result list lookup
    let group_ids = |rep: &PyRep, gid: i64| -> Vec<PyRep> {
        let PyRep::Dict(entries) = rep else {
            panic!("expected search result dict");
        };
        let Some(PyRep::List(ids)) = entries.iter().find_map(|(k, v)| (k.as_i64() == Some(gid)).then_some(v)) else {
            panic!("expected group {gid} list");
        };
        ids.clone()
    };

    // Characters (2), corps (3), alliances (4), structures (11) with ACL filtering
    let char_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "search",
            None,
            "Query",
            &[PyRep::String(char_name), PyRep::List(vec![PyRep::Integer(2)])],
            &empty_kw,
            None,
        )
        .await
        .expect("search.Query character");
    assert!(group_ids(&char_rep, 2).contains(&PyRep::Integer(90_000_001)));

    let starlight_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "search",
            None,
            "Query",
            &[
                PyRep::String("Starlight".into()),
                PyRep::List(vec![PyRep::Integer(3), PyRep::Integer(11)]),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("search.Query corp & structures");
    assert!(group_ids(&starlight_rep, 3).contains(&PyRep::Integer(98_000_222)));
    let struct_hits = group_ids(&starlight_rep, 11);
    assert!(
        struct_hits.contains(&PyRep::Integer(allowed_struct_id)),
        "ACL-accessible corp structure must appear in search.Query"
    );
    assert!(
        !struct_hits.contains(&PyRep::Integer(denied_struct_id)),
        "ACL-denied private structure must be omitted from search.Query"
    );

    // Solar systems (7), stations (9), and published inventory types (10)
    let jita_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "search",
            None,
            "Query",
            &[
                PyRep::String("Jita".into()),
                PyRep::List(vec![PyRep::Integer(7), PyRep::Integer(9)]),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("search.Query Jita");
    assert!(group_ids(&jita_rep, 7).contains(&PyRep::Integer(30_000_142)));
    assert!(group_ids(&jita_rep, 9).contains(&PyRep::Integer(60_003_760)));

    let type_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "search",
            None,
            "Query",
            &[
                PyRep::String("Tritanium".into()),
                PyRep::List(vec![PyRep::Integer(10)]),
                PyRep::Bool(true),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("search.Query exact type");
    assert!(group_ids(&type_rep, 10).contains(&PyRep::Integer(34)));

    // Exact match miss vs hit + structure ACL filter via QuickQuery
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

    let PyRep::List(quick_structs) = ctx
        .dispatch_rpc(
            &mut sess,
            "search",
            None,
            "QuickQuery",
            &[
                PyRep::String("Starlight".into()),
                PyRep::List(vec![PyRep::Integer(11)]),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("QuickQuery structures")
    else {
        panic!("expected QuickQuery structure list");
    };
    assert!(quick_structs.contains(&PyRep::Integer(allowed_struct_id)));
    assert!(
        !quick_structs.contains(&PyRep::Integer(denied_struct_id)),
        "ACL-denied private structure must be omitted from search.QuickQuery"
    );
}
