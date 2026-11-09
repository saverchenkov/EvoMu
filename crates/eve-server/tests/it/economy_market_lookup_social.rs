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
            &[PyRep::String(char_name.clone()), PyRep::List(vec![PyRep::Integer(2)])],
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

    // hideNPC=1 must drop NPC agents (group 1), NPC corporations (group 3), and factions (group 5)
    // while keeping player characters (group 2), player corporations (group 3), and player alliances (group 4).
    let sample_agent = ctx.sde.agents.values().find(|a| a.name.len() >= 3).expect("SDE agent");
    let sample_npc_corp = ctx.sde.npc_corporations.values().find(|c| c.name.len() >= 3).expect("SDE NPC corp");
    let sample_faction = ctx.sde.factions.values().find(|f| f.name.len() >= 3).expect("SDE faction");

    for (query_str, grp_id, expected_id) in [
        (sample_agent.name.clone(), 1i64, sample_agent.agent_id),
        (sample_npc_corp.name.clone(), 3i64, sample_npc_corp.corporation_id),
        (sample_faction.name.clone(), 5i64, i64::from(sample_faction.faction_id)),
    ] {
        let with_npc = ctx
            .dispatch_rpc(&mut sess, "search", None, "Query", &[PyRep::String(query_str.clone()), PyRep::List(vec![PyRep::Integer(grp_id)]), PyRep::Integer(0), PyRep::Integer(0)], &empty_kw, None)
            .await
            .expect("Query hideNPC=0");
        assert!(group_ids(&with_npc, grp_id).contains(&PyRep::Integer(expected_id)));

        let hide_npc = ctx
            .dispatch_rpc(&mut sess, "search", None, "Query", &[PyRep::String(query_str), PyRep::List(vec![PyRep::Integer(grp_id)]), PyRep::Integer(0), PyRep::Integer(1)], &empty_kw, None)
            .await
            .expect("Query hideNPC=1");
        assert!(!group_ids(&hide_npc, grp_id).contains(&PyRep::Integer(expected_id)));
    }

    for (query_str, grp_id, expected_id) in [
        (char_name, 2i64, 90_000_001i64),
        ("Vanguard Logistics".into(), 3i64, 98_000_001i64),
        ("Nebula Accord".into(), 4i64, 99_000_222i64),
    ] {
        let hide_npc = ctx
            .dispatch_rpc(&mut sess, "search", None, "Query", &[PyRep::String(query_str), PyRep::List(vec![PyRep::Integer(grp_id)]), PyRep::Integer(0), PyRep::Integer(1)], &empty_kw, None)
            .await
            .expect("Query player hideNPC=1");
        assert!(group_ids(&hide_npc, grp_id).contains(&PyRep::Integer(expected_id)));
    }
}

pub(crate) mod milestone_logon_browser_lockdown_policy {
    // Live RPC integration tests for character logon milestones, browser lockdown policy,
    // paperdoll apparel inventory invariants, Neocom badges, star map feeds, and map history stats.

    use crate::common;
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::{NotificationRecord, ServerContext};
    use std::collections::HashMap;

    async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, meth: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, svc, None, meth, args, &HashMap::new(), None).await
    }

    fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
        match rep {
            PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| matches!(k, PyRep::String(s) if s == key).then_some(v)),
            PyRep::KeyVal(entries) => entries.iter().find_map(|(k, v)| (k == key).then_some(v)),
            PyRep::Object { args, .. } => dict_get(args, key),
            _ => None,
        }
    }

    fn extract_rowset_lines(rep: &PyRep) -> Vec<Vec<PyRep>> {
        match rep { PyRep::Rowset { rows, .. } => rows.clone(), other => panic!("expected PyRep::Rowset, got {other:?}") }
    }

    fn crowset_row_vals(rep: &PyRep, sys_id: i64) -> Option<(i64, i64, i64)> {
        let PyRep::ObjectEx { list, .. } = rep else { panic!("expected CRowset ObjectEx, got {rep:?}"); };
        list.iter().find_map(|row| match row {
            PyRep::DBRow { columns, .. } => {
                let sid = columns.iter().find_map(|(k, v)| (k == "solarSystemID").then(|| v.as_i64()).flatten())?;
                if sid != sys_id { return None; }
                let v1 = columns.iter().find_map(|(k, v)| (k == "value1").then(|| v.as_i64()).flatten()).unwrap_or(0);
                let v2 = columns.iter().find_map(|(k, v)| (k == "value2").then(|| v.as_i64()).flatten()).unwrap_or(0);
                let v3 = columns.iter().find_map(|(k, v)| (k == "value3").then(|| v.as_i64()).flatten()).unwrap_or(0);
                Some((v1, v2, v3))
            }
            _ => None,
        })
    }

    #[tokio::test]
    async fn test_milestone_logon_and_browser_lockdown_policy() {
        // parity: feat:ui.browser-lockdown
        let ctx = ServerContext::new();
        ctx.seed_demo_world_fixtures();
        common::pin_clock(&ctx);
        let mut unauth = SessionState::new(4106);
        assert!(rpc(&ctx, &mut unauth, "milestoneMgr", "ProcessCharacterLogon", &[]).await.is_err());
        assert!(rpc(&ctx, &mut unauth, "browserLockdownSvc", "IsBrowserInLockdown", &[]).await.is_err());

        let mut sess = SessionState::new(4107);
        sess.user_id = Some(1);
        sess.character_id = Some(90_000_001);
        rpc(&ctx, &mut sess, "milestoneMgr", "ProcessCharacterLogon", &[])
            .await
            .expect("ProcessCharacterLogon");
        let first_logon_ts = ctx
            .db
            .with_snapshot(|snap| {
                eve_server::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap)
                    .milestone_logons
                    .get(&90_000_001)
                    .copied()
            })
            .expect("milestone_logons must record character logon");
        assert!(first_logon_ts > 0);

        let mut sess_next = SessionState::new(4108);
        sess_next.user_id = Some(1);
        sess_next.character_id = Some(90_000_001);
        rpc(&ctx, &mut sess_next, "milestoneMgr", "ProcessCharacterLogon", &[])
            .await
            .expect("ProcessCharacterLogon second session");
        let second_logon_ts = ctx
            .db
            .with_snapshot(|snap| {
                eve_server::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap)
                    .milestone_logons
                    .get(&90_000_001)
                    .copied()
            })
            .expect("milestone_logons persisted across sessions");
        assert!(second_logon_ts >= first_logon_ts);

        // Default browser policy
        assert_eq!(
            rpc(&ctx, &mut sess, "browserLockdownSvc", "IsBrowserInLockdown", &[]).await.unwrap(),
            PyRep::Bool(false)
        );
        assert_eq!(
            rpc(&ctx, &mut sess, "browserLockdownSvc", "GetDefaultHomePage", &[]).await.unwrap(),
            PyRep::String("https://www.eveonline.com/".into())
        );
        let hash_before = rpc(&ctx, &mut sess, "browserLockdownSvc", "GetFlaggedSitesHash", &[])
            .await
            .expect("GetFlaggedSitesHash before");

        // Update persisted policy with structured (siteUrl, siteFlag) entries per V24 SitesSvc
        ctx.db.with_mut_snapshot(|snap| {
            eve_server::services::notify_emitters_skills_trade_godma::set_browser_lockdown_policy(
                snap,
                true,
                "https://support.eveonline.com/",
                vec![
                    ("https://blocked.example.com".into(), 2),
                    ("https://trusted.example.com".into(), 1),
                ],
            );
        });
        assert_eq!(
            rpc(&ctx, &mut sess, "browserLockdownSvc", "IsBrowserInLockdown", &[]).await.unwrap(),
            PyRep::Bool(true)
        );
        assert_eq!(
            rpc(&ctx, &mut sess, "browserLockdownSvc", "GetDefaultHomePage", &[]).await.unwrap(),
            PyRep::String("https://support.eveonline.com/".into())
        );
        let hash_after = rpc(&ctx, &mut sess, "browserLockdownSvc", "GetFlaggedSitesHash", &[])
            .await
            .expect("GetFlaggedSitesHash after");
        assert_ne!(hash_before, hash_after, "flagged sites hash must change when policy changes");

        let PyRep::List(flagged) = rpc(&ctx, &mut sess, "browserLockdownSvc", "GetFlaggedSitesList", &[])
            .await
            .expect("GetFlaggedSitesList after")
        else {
            panic!("expected List from GetFlaggedSitesList");
        };
        assert_eq!(flagged.len(), 2);
        assert_eq!(dict_get(&flagged[0], "siteUrl"), Some(&PyRep::String("https://blocked.example.com".into())));
        assert_eq!(dict_get(&flagged[0], "siteFlag"), Some(&PyRep::Integer(2)));
        assert_eq!(dict_get(&flagged[1], "siteUrl"), Some(&PyRep::String("https://trusted.example.com".into())));
        assert_eq!(dict_get(&flagged[1], "siteFlag"), Some(&PyRep::Integer(1)));
    }

    #[tokio::test]
    async fn test_cosmetics_apparel_inventory_and_paperdoll_shape() {
        // parity: feat:cosmetics.apparel
        let ctx = ServerContext::new();
        ctx.seed_demo_world_fixtures();
        common::pin_clock(&ctx);
        let mut sess = SessionState::new(4109);
        sess.user_id = Some(1);
        sess.character_id = Some(90_000_001);
        sess.corporation_id = Some(98_000_001);
        sess.station_id = Some(60_003_760);

        // Negative 1: unowned apparel rejected
        let bad_doll = PyRep::keyval([("apparel", PyRep::List(vec![PyRep::Integer(999_999_999)]))]);
        let bad_err = rpc(
            &ctx,
            &mut sess,
            "paperDollServer",
            "UpdateExistingCharacterLimited",
            &[PyRep::Integer(90_000_001), bad_doll, PyRep::None],
        )
        .await
        .expect_err("unowned apparel must fail");
        assert_eq!(bad_err.code, "InvalidApparelItem");

        let apparel_type_id = ctx
            .sde
            .types
            .values()
            .find(|t| t.category_id == 30)
            .map(|t| t.type_id)
            .expect("SDE apparel type (category 30)");
        let remote_item_id = 990_000_775i64;
        let locked_item_id = 990_000_776i64;
        let stack_item_id = 990_000_777i64;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.gender = 1; // male -> GetPaperDollData dna[0] == "male"
            }
            snap.items.insert(
                remote_item_id,
                eve_db::InventoryItemRecord {
                    item_id: remote_item_id,
                    type_id: apparel_type_id,
                    owner_id: 90_000_001,
                    location_id: 60_008_494, // Amarr VIII — remote station
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                locked_item_id,
                eve_db::InventoryItemRecord {
                    item_id: locked_item_id,
                    type_id: apparel_type_id,
                    owner_id: 90_000_001,
                    location_id: 60_003_760,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.contracts.insert(
                770_001,
                eve_db::ContractRecord {
                    contract_id: 770_001,
                    issuer_id: 90_000_001,
                    status: "Outstanding".into(),
                    item_ids: vec![locked_item_id],
                    ..Default::default()
                },
            );
            snap.items.insert(
                stack_item_id,
                eve_db::InventoryItemRecord {
                    item_id: stack_item_id,
                    type_id: apparel_type_id,
                    owner_id: 90_000_001,
                    location_id: 60_003_760,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 3,
                    singleton: false,
                    ..Default::default()
                },
            );
            snap.items.insert(
                990_000_778,
                eve_db::InventoryItemRecord {
                    item_id: 990_000_778,
                    type_id: apparel_type_id,
                    owner_id: 90_000_001,
                    location_id: 60_003_760,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });

        // Negative 2..5: remote-station, contract-locked, undocked, duplicate item, and same wearable group rejected
        let remote_doll = PyRep::keyval([("apparel", PyRep::List(vec![PyRep::Integer(remote_item_id)]))]);
        let remote_err = rpc(&ctx, &mut sess, "paperDollServer", "UpdateExistingCharacterLimited", &[PyRep::Integer(90_000_001), remote_doll, PyRep::None])
            .await
            .expect_err("remote-station apparel must fail");
        assert_eq!(remote_err.code, "InvalidApparelItem");

        let locked_doll = PyRep::keyval([("apparel", PyRep::List(vec![PyRep::Integer(locked_item_id)]))]);
        let locked_err = rpc(&ctx, &mut sess, "paperDollServer", "UpdateExistingCharacterLimited", &[PyRep::Integer(90_000_001), locked_doll, PyRep::None])
            .await
            .expect_err("contract-locked apparel must fail");
        assert_eq!(locked_err.code, "ItemLockedInContractOrJob");

        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&90_000_001).unwrap().station_id = None);
        let stack_doll = PyRep::keyval([
            ("apparel", PyRep::List(vec![PyRep::Integer(stack_item_id)])),
            ("colors", PyRep::keyval([("skin", PyRep::Integer(7))])),
        ]);
        let undocked_err = rpc(&ctx, &mut sess, "paperDollServer", "UpdateExistingCharacterLimited", &[PyRep::Integer(90_000_001), stack_doll.clone(), PyRep::None])
            .await
            .expect_err("undocked apparel equip must fail");
        assert_eq!(undocked_err.code, "NotDocked");

        // Positive: docked equip splits stack of 3 -> 2 in hangar + 1 singleton in Wardrobe (flag 3), emits OnItemChange
        assert_eq!(eve_db::InventoryFlag::Wardrobe as u16, 3);
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&90_000_001).unwrap().station_id = Some(60_003_760));
        let dup_doll = PyRep::keyval([("apparel", PyRep::List(vec![PyRep::Integer(stack_item_id), PyRep::Integer(stack_item_id)]))]);
        assert_eq!(rpc(&ctx, &mut sess, "paperDollServer", "UpdateExistingCharacterLimited", &[PyRep::Integer(90_000_001), dup_doll, PyRep::None]).await.unwrap_err().code, "InvalidApparelItem");
        let same_grp_doll = PyRep::keyval([("apparel", PyRep::List(vec![PyRep::Integer(stack_item_id), PyRep::Integer(990_000_778)]))]);
        assert_eq!(rpc(&ctx, &mut sess, "paperDollServer", "UpdateExistingCharacterLimited", &[PyRep::Integer(90_000_001), same_grp_doll, PyRep::None]).await.unwrap_err().code, "InvalidApparelItem");
        let _ = sess.drain_notifications();
        rpc(&ctx, &mut sess, "paperDollServer", "UpdateExistingCharacterLimited", &[PyRep::Integer(90_000_001), stack_doll, PyRep::None])
            .await
            .expect("UpdateExistingCharacterLimited stack split");

        assert!(
            sess.drain_notifications().iter().any(|(name, _)| name == "OnItemChange"),
            "equipping apparel must emit OnItemChange"
        );
        let split_item_id = ctx.db.with_snapshot(|snap| {
            let rem = snap.items.get(&stack_item_id).expect("remaining hangar stack");
            assert_eq!((rem.quantity, rem.location_id, rem.flag), (2, 60_003_760, eve_db::InventoryFlag::Hangar));
            snap.items
                .values()
                .find(|i| {
                    i.owner_id == 90_000_001
                        && i.type_id == apparel_type_id
                        && i.location_id == 90_000_001
                        && i.flag == eve_db::InventoryFlag::Wardrobe
                        && i.quantity == 1
                        && i.singleton
                })
                .map(|i| i.item_id)
                .expect("split singleton wardrobe item")
        });

        // Verify GetMyPaperDollData & GetPaperDollData (KeyVal for V24 ApplyDBRowToDoll) persistence
        let my_doll = rpc(&ctx, &mut sess, "paperDollServer", "GetMyPaperDollData", &[])
            .await
            .expect("GetMyPaperDollData");
        assert!(dict_get(&my_doll, "dollInfo").and_then(|v| v.as_str()).unwrap().contains("colors"));
        assert_eq!(
            dict_get(&my_doll, "wornApparel"),
            Some(&PyRep::List(vec![PyRep::Integer(i64::from(apparel_type_id))]))
        );

        let public_doll = rpc(
            &ctx,
            &mut sess,
            "paperDollServer",
            "GetPaperDollData",
            &[PyRep::Integer(90_000_001)],
        )
        .await
        .expect("GetPaperDollData");
        assert!(matches!(public_doll, PyRep::KeyVal(_)), "expected KeyVal from GetPaperDollData");
        assert_eq!(dict_get(&public_doll, "genderLabel"), Some(&PyRep::String("Male".into())));
        assert_eq!(
            dict_get(&public_doll, "wornApparel"),
            Some(&PyRep::List(vec![PyRep::Integer(i64::from(apparel_type_id))]))
        );

        // Unequip moves singleton back to docked station hangar (60_003_760, Hangar)
        let unwear = PyRep::keyval([("apparel", PyRep::List(vec![]))]);
        rpc(
            &ctx,
            &mut sess,
            "paperDollServer",
            "UpdateExistingCharacterLimited",
            &[PyRep::Integer(90_000_001), unwear, PyRep::None],
        )
        .await
        .expect("UpdateExistingCharacterLimited unequip");
        ctx.db.with_snapshot(|snap| {
            let returned = snap.items.get(&split_item_id).expect("returned apparel item");
            assert_eq!((returned.location_id, returned.flag), (60_003_760, eve_db::InventoryFlag::Hangar));
        });
    }

    #[tokio::test]
    async fn test_neocom_badges_mail_notifications_and_login_skills() {
        // parity: feat:ui.neocom-badges
        let ctx = ServerContext::new();
        ctx.seed_demo_world_fixtures();
        common::pin_clock(&ctx);
        let now_unix = ctx.now_unix();

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.skill_points = 512_000.0;
            }
            snap.skill_queues.insert(
                90_000_001,
                vec![eve_db::SkillQueueEntry {
                    type_id: 3300, // Gunnery
                    target_level: 2,
                    start_sp: 250.0,
                    end_sp: 1_415.0,
                    duration_seconds: 1_800.0,
                    started_at_unix: Some(now_unix),
                }],
            );
        });
        let notif_id = 990_441i64;
        ctx.notification_records.write().unwrap().push(NotificationRecord {
            notification_id: notif_id,
            type_id: 101,
            sender_id: 1_000_125,
            receiver_id: 90_000_001,
            processed: false,
            group_id: 12,
            created: now_unix as i64,
            data: "badgeTest: 1\n".into(),
            deleted: false,
            interacted: false,
        });

        let mut sender_sess = SessionState::new(4110);
        sender_sess.user_id = Some(2);
        sender_sess.character_id = Some(90_000_002);
        let mail_id = rpc(
            &ctx,
            &mut sender_sess,
            "mailMgr",
            "SendMail",
            &[
                PyRep::List(vec![PyRep::Integer(90_000_001)]),
                PyRep::String("Neocom Badge Test".into()),
                PyRep::String("Unread mail badge check".into()),
                PyRep::Integer(0),
                PyRep::Integer(0),
            ],
        )
        .await
        .expect("SendMail")
        .as_i64()
        .expect("mail_id");

        // 1. Login character-selection data exposes unread mail count and active skill training state
        let mut login_sess = SessionState::new(4111);
        login_sess.user_id = Some(1);
        let PyRep::Tuple(charsel_before) = rpc(&ctx, &mut login_sess, "charUnboundMgr", "GetCharacterSelectionData", &[])
            .await
            .expect("GetCharacterSelectionData before")
        else {
            panic!("expected Tuple");
        };
        let PyRep::List(char_rows) = &charsel_before[2] else {
            panic!("expected character selection rows");
        };
        let p1_row = char_rows
            .iter()
            .find(|r| dict_get(r, "characterID") == Some(&PyRep::Integer(90_000_001)))
            .expect("character 90000001 row");
        let unread_before = dict_get(p1_row, "unreadMailCount").and_then(|v| v.as_i64()).unwrap_or(0);
        assert!(unread_before >= 1, "login unreadMailCount must reflect new unread mail");
        assert_eq!(dict_get(p1_row, "skillPoints"), Some(&PyRep::Real(512_000.0)));
        assert_eq!(dict_get(p1_row, "skillTypeID"), Some(&PyRep::Integer(3300)));
        assert_eq!(dict_get(p1_row, "toLevel"), Some(&PyRep::Integer(2)));

        // 2. Character session verifies mailMgr, notificationMgr, and skillMgr2 badge feeds + state transitions
        let mut sess = SessionState::new(4112);
        sess.user_id = Some(1);
        sess.character_id = Some(90_000_001);

        let hdrs_before = extract_rowset_lines(&rpc(&ctx, &mut sess, "mailMgr", "GetMailHeaders", &[]).await.unwrap());
        assert!(
            hdrs_before
                .iter()
                .any(|r| r.first() == Some(&PyRep::Integer(mail_id)) && r.get(7) == Some(&PyRep::Bool(false)))
        );
        rpc(&ctx, &mut sess, "mailMgr", "MarkAsRead", &[PyRep::List(vec![PyRep::Integer(mail_id)])])
            .await
            .expect("MarkAsRead");

        let PyRep::Tuple(charsel_after) = rpc(&ctx, &mut login_sess, "charUnboundMgr", "GetCharacterSelectionData", &[])
            .await
            .unwrap()
        else {
            panic!("expected Tuple");
        };
        let PyRep::List(char_rows_after) = &charsel_after[2] else { panic!("expected rows") };
        let p1_after = char_rows_after
            .iter()
            .find(|r| dict_get(r, "characterID") == Some(&PyRep::Integer(90_000_001)))
            .unwrap();
        assert_eq!(dict_get(p1_after, "unreadMailCount").and_then(|v| v.as_i64()), Some(unread_before - 1));

        let unproc_before = rpc(&ctx, &mut sess, "notificationMgr", "GetUnprocessed", &[]).await.unwrap();
        assert!(format!("{unproc_before:?}").contains(&notif_id.to_string()));
        rpc(&ctx, &mut sess, "notificationMgr", "MarkAsProcessed", &[PyRep::List(vec![PyRep::Integer(notif_id)])])
            .await
            .expect("MarkAsProcessed");
        let unproc_after = rpc(&ctx, &mut sess, "notificationMgr", "GetUnprocessed", &[]).await.unwrap();
        assert!(!format!("{unproc_after:?}").contains(&notif_id.to_string()));

        let skills_rows = extract_rowset_lines(&rpc(&ctx, &mut sess, "skillMgr2", "GetSkills", &[]).await.unwrap());
        assert!(!skills_rows.is_empty());
        let PyRep::Tuple(queue_and_free) = rpc(&ctx, &mut sess, "skillMgr2", "GetSkillQueueAndFreePoints", &[])
            .await
            .unwrap()
        else {
            panic!("expected Tuple from GetSkillQueueAndFreePoints");
        };
        let PyRep::List(queue_entries) = &queue_and_free[0] else {
            panic!("expected List of KeyVals from GetSkillQueueAndFreePoints[0]");
        };
        assert_eq!(queue_entries.len(), 1);
    }

    #[tokio::test]
    async fn test_star_map_data_feeds_and_fw_zone_upgrades() {
        // parity: feat:ui.star-map-data, rpc:map.GetFacWarZoneInfo
        let ctx = ServerContext::new();
        ctx.seed_demo_world_fixtures();
        common::pin_clock(&ctx);

        ctx.db.with_mut_snapshot(|s| {
            let ch2 = s.characters.get_mut(&90_000_002).unwrap();
            ch2.station_id = None;
            ch2.solar_system_id = 32_000_001;
        });
        let mut abyss_sess = SessionState::new(4113);
        abyss_sess.user_id = Some(2);
        abyss_sess.character_id = Some(90_000_002);
        let abyss_err = rpc(&ctx, &mut abyss_sess, "map", "GetBeaconCount", &[]).await.unwrap_err();
        assert_eq!(abyss_err.code, "MapDisabledInAbyssalSpace");

        let mut sess = SessionState::new(4114);
        sess.user_id = Some(1);
        sess.character_id = Some(90_000_001);
        sess.solar_system_id = Some(30_000_142);

        // 1. Beacon counts
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(
                1_030_000_358_401,
                eve_db::UpwellStructureRecord {
                    structure_id: 1_030_000_358_401,
                    type_id: 35840,
                    owner_corp_id: 98_000_001,
                    solar_system_id: 30_000_142,
                    name: "Jita Cyno Beacon".into(),
                    state: "ShieldVulnerable".into(),
                    ..Default::default()
                },
            );
        });
        let PyRep::Dict(beacons) = rpc(&ctx, &mut sess, "map", "GetBeaconCount", &[]).await.unwrap() else {
            panic!("expected Dict from GetBeaconCount");
        };
        assert!(beacons.iter().any(|(k, v)| k == &PyRep::Integer(30_000_142) && matches!(v, PyRep::Tuple(t) if t.len() == 2 && t[1].as_i64().unwrap_or(0) >= 1)));

        // 2. Incursion global report & per-system incursion map
        let PyRep::List(inc_reports) = rpc(&ctx, &mut sess, "map", "GetIncursionGlobalReport", &[]).await.unwrap() else {
            panic!("expected List");
        };
        assert!(!inc_reports.is_empty() && dict_get(&inc_reports[0], "stagingSolarSystemID").is_some());
        let PyRep::List(inc_systems) = rpc(&ctx, &mut sess, "map", "GetSystemsInIncursions", &[]).await.unwrap() else {
            panic!("expected List");
        };
        assert!(!inc_systems.is_empty());

        // 3. FW zone info: verify systemUpgradeLevel Dict and faction warzone VP isolation
        let fw_cal_init = rpc(&ctx, &mut sess, "map", "GetFacWarZoneInfo", &[PyRep::Integer(500_001)]).await.unwrap();
        let fw_amarr_init = rpc(&ctx, &mut sess, "map", "GetFacWarZoneInfo", &[PyRep::Integer(500_002)]).await.unwrap();
        let first_wz_sys = |rep: &PyRep| -> u32 {
            let Some(PyRep::List(ids)) = dict_get(rep, "solarSystemIds") else { panic!("expected solarSystemIds") };
            ids.first().and_then(|v| v.as_i64()).expect("warzone system id") as u32
        };
        let cal_sys = first_wz_sys(&fw_cal_init);
        let amarr_sys = first_wz_sys(&fw_amarr_init);
        ctx.db.with_mut_snapshot(|snap| {
            snap.fw_runtime.system_victory_points.clear();
            snap.fw_runtime.ihub_tiers.clear();
            snap.fw_runtime.system_victory_points.insert(cal_sys, 1_250);
            snap.fw_runtime.system_victory_points.insert(amarr_sys, 9_999);
            snap.fw_runtime.ihub_tiers.insert(cal_sys, 4);
        });
        let fw_info = rpc(&ctx, &mut sess, "map", "GetFacWarZoneInfo", &[PyRep::Integer(500_001)]).await.unwrap();
        assert_eq!(dict_get(&fw_info, "maxWarZonePoints"), Some(&PyRep::Integer(100_000)));
        assert_eq!(dict_get(&fw_info, "victoryPoints"), Some(&PyRep::Integer(1_250)));
        assert_eq!(dict_get(&fw_info, "factionPoints"), Some(&PyRep::Integer(1_250)));
        let Some(PyRep::Dict(upgrade_levels)) = dict_get(&fw_info, "systemUpgradeLevel") else {
            panic!("expected systemUpgradeLevel Dict");
        };
        assert!(upgrade_levels.iter().any(|(k, v)| k == &PyRep::Integer(i64::from(cal_sys)) && v == &PyRep::Integer(4)));

        // 4. EDENCOM & Triglavian star map feeds
        let PyRep::List(fortresses) = rpc(&ctx, &mut sess, "map", "GetEdencomFortressSystems", &[]).await.unwrap() else {
            panic!("expected List");
        };
        let PyRep::List(eden_min) = rpc(&ctx, &mut sess, "map", "GetEdencomMinorVictorySystems", &[]).await.unwrap() else {
            panic!("expected List");
        };
        let PyRep::List(trig_min) = rpc(&ctx, &mut sess, "map", "GetTriglavianMinorVictorySystems", &[]).await.unwrap()
        else {
            panic!("expected List");
        };
        assert!(!fortresses.is_empty() && !eden_min.is_empty() && !trig_min.is_empty());

        // 5. Deadspace complex & agent maps: 4-column (solarSystemID, dungeonID, difficulty, dungeonName)
        let complexes = rpc(&ctx, &mut sess, "map", "GetDeadspaceComplexMap", &[]).await.unwrap();
        let complex_rows = complexes.crowset_rows().expect("complex crowset");
        assert!(matches!(complex_rows.first(), Some(PyRep::DBRow { columns, .. }) if columns.len() == 4 && columns[0].0 == "solarSystemID" && columns[3].0 == "dungeonName"));

        let now_unix = ctx.now_unix();
        let mk_esc = |id: i64, cid: i64, did: u32, name: &str, sys: u32, step: u8| eve_server::services::pve_exploration::EscalatingPathEntry { instance_id: id, character_id: cid, source_dungeon_id: 1001, expedition_dungeon_id: did, expedition_name: name.into(), destination_solar_system_id: sys, step_index: step, total_steps: 4, journal_text: name.into(), created_at_sec: now_unix, expires_at_sec: now_unix + 86_400, dungeons_entered: vec![1001], journal_entry_template_id: 280_000 + u32::from(step) };
        eve_server::services::pve_exploration::with_pve_exploration_reg_db(&ctx.db, now_unix, |reg| {
            reg.escalations_by_char.entry(90_000_001).or_default().push(mk_esc(910_001, 90_000_001, 2001, "Caller Escalation", 30_000_142, 2));
            reg.escalations_by_char.entry(90_000_002).or_default().push(mk_esc(910_002, 90_000_002, 2002, "Other Pilot Escalation", 30_002_187, 1));
        });
        let agents = rpc(&ctx, &mut sess, "map", "GetDeadspaceAgentsMap", &[]).await.unwrap();
        let agent_rows = agents.crowset_rows().expect("agents crowset");
        assert_eq!(agent_rows.len(), 1);
        assert!(matches!(agent_rows.first(), Some(PyRep::DBRow { columns, .. }) if columns.len() == 4 && columns[0] == ("solarSystemID".into(), PyRep::Integer(30_000_142)) && columns[1] == ("dungeonID".into(), PyRep::Integer(2001)) && columns[2] == ("difficulty".into(), PyRep::Integer(2)) && columns[3] == ("dungeonName".into(), PyRep::String("Caller Escalation".into()))));
    }

    #[tokio::test]
    async fn test_map_history_jumps_kills_timeframes_and_visit_stats() {
        // parity: feat:ui.map-stats, rpc:map.GetHistory
        let ctx = ServerContext::new();
        ctx.seed_demo_world_fixtures();
        common::pin_clock(&ctx);
        let now_unix = ctx.now_unix();

        let mut unauth = SessionState::new(4115);
        assert!(rpc(&ctx, &mut unauth, "map", "GetHistory", &[PyRep::Integer(1), PyRep::Integer(1)]).await.is_err());

        let mut sess = SessionState::new(4116);
        sess.user_id = Some(1);
        sess.character_id = Some(90_000_001);
        sess.solar_system_id = Some(30_000_142);

        // Station counts & industry cost indices on map service
        let expected_jita_stations = ctx.sde.stations.values().filter(|s| s.solar_system_id == 30_000_142).count() as i64;
        let PyRep::Dict(stn_counts) = rpc(&ctx, &mut sess, "map", "GetStationCount", &[]).await.unwrap() else {
            panic!("expected Dict from GetStationCount");
        };
        assert!(stn_counts.iter().any(|(k, v)| k == &PyRep::Integer(30_000_142) && v == &PyRep::Integer(expected_jita_stations)));
        let cost_mod = rpc(&ctx, &mut sess, "map", "GetIndustryCostModifier", &[PyRep::Integer(30_000_142)]).await.unwrap();
        assert!(dict_get(&cost_mod, "manufacturing").and_then(|v| v.as_f64()).is_some_and(|x| x >= 0.0));

        // Record 2 live recent jumps via record_solar_system_jump + 3 jumps in 24h window + 1 expired (>24h)
        eve_server::services::map_service::record_solar_system_jump(&ctx, 90_000_001, 30_000_142);
        eve_server::services::map_service::record_solar_system_jump(&ctx, 90_000_001, 30_000_142);
        ctx.db.with_mut_snapshot(|snap| {
            for _ in 0..3 {
                snap.record_system_jump(30_000_142, 90_000_002, now_unix - 7_200);
            }
            snap.record_system_jump(30_000_142, 90_000_002, now_unix - 100_000);
            for (kid, victim, stype, ts) in [
                (990_101, 90_000_002, 587u32, now_unix - 600),
                (990_102, 90_000_002, 587, now_unix - 7_200),
                (990_103, 90_000_002, 587, now_unix - 100_000),
                (990_104, 90_000_002, 670, now_unix - 300),
                (990_105, 0, 587, now_unix - 300),
            ] {
                snap.killmails.insert(
                    kid,
                    eve_db::KillmailRecord {
                        kill_id: kid,
                        victim_character_id: victim,
                        attacker_character_id: 90_000_001,
                        solar_system_id: 30_000_142,
                        ship_type_id: stype,
                        killed_unix: ts,
                        ..Default::default()
                    },
                );
            }
            snap.record_system_kill(30_000_142, 0, 2, 0, now_unix - 300);
            snap.record_system_kill(30_000_142, 0, 2, 0, now_unix - 7_200);
        });

        let PyRep::List(visits) = rpc(&ctx, &mut sess, "map", "GetSolarSystemVisits", &[]).await.unwrap() else {
            panic!("expected visits list");
        };
        assert!(visits.iter().any(|v| {
            dict_get(v, "solarSystemID") == Some(&PyRep::Integer(30_000_142))
                && dict_get(v, "visits") == Some(&PyRep::Integer(2))
        }));

        // stat_type == 1 (Jumps), 2 (Ship/NPC/Pod kills), 3 (MostActive), 4 (PodKills), 5 (NpcKills) across 1h vs 24h
        let jumps_1h = rpc(&ctx, &mut sess, "map", "GetHistory", &[PyRep::Integer(1), PyRep::Integer(1)]).await.unwrap();
        assert_eq!(crowset_row_vals(&jumps_1h, 30_000_142), Some((2, 0, 0)));
        let jumps_24h = rpc(&ctx, &mut sess, "map", "GetHistory", &[PyRep::Integer(1), PyRep::Integer(24)]).await.unwrap();
        assert_eq!(crowset_row_vals(&jumps_24h, 30_000_142), Some((5, 0, 0)));

        let kills_1h = rpc(&ctx, &mut sess, "map", "GetHistory", &[PyRep::Integer(2), PyRep::Integer(1)]).await.unwrap();
        assert_eq!(crowset_row_vals(&kills_1h, 30_000_142), Some((4, 3, 1)));
        let kills_24h = rpc(&ctx, &mut sess, "map", "GetHistory", &[PyRep::Integer(2), PyRep::Integer(24)]).await.unwrap();
        assert_eq!(crowset_row_vals(&kills_24h, 30_000_142), Some((7, 5, 1)));

        let active_1h = rpc(&ctx, &mut sess, "map", "GetHistory", &[PyRep::Integer(3), PyRep::Integer(1)]).await.unwrap();
        assert_eq!(crowset_row_vals(&active_1h, 30_000_142), Some((1, 0, 0)));
        let active_24h = rpc(&ctx, &mut sess, "map", "GetHistory", &[PyRep::Integer(3), PyRep::Integer(24)]).await.unwrap();
        assert_eq!(crowset_row_vals(&active_24h, 30_000_142), Some((2, 0, 0)));

        let pods_1h = rpc(&ctx, &mut sess, "map", "GetHistory", &[PyRep::Integer(4), PyRep::Integer(1)]).await.unwrap();
        assert_eq!(crowset_row_vals(&pods_1h, 30_000_142), Some((1, 0, 1)));

        let npc_1h = rpc(&ctx, &mut sess, "map", "GetHistory", &[PyRep::Integer(5), PyRep::Integer(1)]).await.unwrap();
        assert_eq!(crowset_row_vals(&npc_1h, 30_000_142), Some((3, 3, 0)));
        let npc_24h = rpc(&ctx, &mut sess, "map", "GetHistory", &[PyRep::Integer(5), PyRep::Integer(24)]).await.unwrap();
        assert_eq!(crowset_row_vals(&npc_24h, 30_000_142), Some((5, 5, 0)));
    }

    #[test]
    fn test_record_system_jump_and_kill_prunes_events_older_than_24h() {
        let ctx = ServerContext::new();
        let now = 1_700_200_000u64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.record_system_jump(30_000_142, 90_000_001, now - 86_401);
            snap.record_system_jump(30_000_142, 90_000_002, now - 86_400);
            snap.record_system_jump(30_000_142, 90_000_003, now);
            assert_eq!(snap.system_jump_events.len(), 2);
            snap.record_system_kill(30_000_142, 1, 2, 0, now - 86_401);
            snap.record_system_kill(30_000_142, 3, 4, 1, now - 86_400);
            snap.record_system_kill(30_000_142, 5, 6, 2, now);
            assert_eq!(snap.system_kill_events.len(), 2);
        });
    }
}

