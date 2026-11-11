//! Extra unit and mutation-killing tests for `services::market_trading`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::market_contracts::{JITA_STATION_ID, PLEX_TYPE_ID, THE_FORGE_REGION_ID};
use crate::services::market_trading::*;
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
    OwnerNoteRecord, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn test_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn jita_session(char_id: i64, corp_id: i64) -> SessionState {
    let mut s = SessionState::new(9301);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.station_id = Some(JITA_STATION_ID);
    s.solar_system_id = Some(30_000_142);
    s.region_id = Some(THE_FORGE_REGION_ID);
    s
}

#[test]
fn meta_fallbacks_unix_filetime_sales_tax_and_summary_rows() {
    let ctx = test_ctx();
    let now = 1_750_000_000_u64;

    assert_eq!(
        unix_to_filetime(1_700_000_000),
        (1_700_000_000_i64 + 11_644_473_600) * 10_000_000
    );

    // Sales tax at Accounting 0 (7.5% = 7500 cents on 1000 ISK) vs Accounting 5 (3.375% = 3375 cents on 1000 ISK)
    assert_eq!(
        compute_sales_tax_cents(IskCents::from_isk(1_000), 0),
        IskCents(7_500)
    );
    assert_eq!(
        compute_sales_tax_cents(IskCents::from_isk(1_000), 5),
        IskCents(3_375)
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut ord = MarketOrderRecord {
            order_id: 850_001,
            owner_id: 90_000_001,
            station_id: JITA_STATION_ID,
            region_id: THE_FORGE_REGION_ID,
            type_id: 34,
            is_bid: true,
            price: IskCents::from_isk(10),
            vol_entered: 50,
            vol_remaining: 50,
            min_volume: 1,
            is_npc: false,
            escrow: IskCents::from_isk(500),
        };

        // Fallback when neither owner_notes nor market_order_meta exists
        let def_bid = load_market_order_meta(snap, &ord, now);
        assert_eq!(def_bid.range, -1);
        assert_eq!(def_bid.duration, 90);

        ord.is_bid = false;
        let def_ask = load_market_order_meta(snap, &ord, now);
        assert_eq!(def_ask.range, 32767);
        assert_eq!(def_ask.duration, 90);

        ord.is_npc = true;
        let def_npc = load_market_order_meta(snap, &ord, now);
        assert_eq!(def_npc.range, 32767);
        assert_eq!(def_npc.duration, 365);
        ord.is_npc = false;
        ord.is_bid = true;

        // Save meta with solarsystem range (0) and expired state (2), then remove owner_note to test market_order_meta fallback
        save_market_order_meta(
            snap,
            850_001,
            MarketOrderMeta {
                range: 0,
                duration: 14,
                issue_unix: now - 50,
                order_state: Some(2),
                account_key: Some(1003),
                final_vol_remaining: Some(0),
                closed_unix: Some(now),
            },
        );
        assert_eq!(snap.market_order_meta[&850_001].range, "solarsystem");
        assert_eq!(
            snap.market_order_meta[&850_001].state.as_deref(),
            Some("expired")
        );
        assert_eq!(snap.market_order_meta[&850_001].wallet_division, Some(4));

        snap.owner_notes.clear();
        let from_esi = load_market_order_meta(snap, &ord, now);
        assert_eq!(from_esi.range, 0);
        assert_eq!(from_esi.duration, 14);
        assert_eq!(from_esi.order_state, Some(4));
        assert_eq!(from_esi.account_key, Some(1003));

        // Test cancelled state (3) and numeric jump range (5) via market_order_meta fallback
        save_market_order_meta(
            snap,
            850_001,
            MarketOrderMeta {
                range: 5,
                duration: 30,
                issue_unix: now - 10,
                order_state: Some(3),
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
        assert_eq!(snap.market_order_meta[&850_001].range, "5");
        assert_eq!(
            snap.market_order_meta[&850_001].state.as_deref(),
            Some("cancelled")
        );
        snap.owner_notes.clear();
        let from_esi2 = load_market_order_meta(snap, &ord, now);
        assert_eq!(from_esi2.range, 5);
        assert_eq!(from_esi2.order_state, Some(3));

        // Solarsystem range (0): matches different structure in Jita (30000142), rejects Perimeter (30000144)
        let jita_struct = 1_045_000_001_i64;
        snap.structures.insert(
            jita_struct,
            UpwellStructureRecord {
                structure_id: jita_struct,
                type_id: 35834,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_142,
                name: "Jita Fort".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        save_market_order_meta(
            snap,
            850_001,
            MarketOrderMeta {
                range: 0,
                duration: 30,
                issue_unix: now,
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
        assert!(can_bid_match_seller_station(&ctx, snap, &ord, jita_struct, now));
        let perim_station = ctx.sde.solar_systems[&30_000_144].stations[0].station_id;
        assert!(!can_bid_match_seller_station(
            &ctx,
            snap,
            &ord,
            perim_station,
            now
        ));
    });

    // best_asks_to_summary_rows picks lowest ask per type
    let o1 = MarketOrderRecord {
        order_id: 1,
        owner_id: 90_000_001,
        station_id: JITA_STATION_ID,
        region_id: THE_FORGE_REGION_ID,
        type_id: 34,
        is_bid: false,
        price: IskCents::from_isk(12),
        vol_entered: 10,
        vol_remaining: 10,
        min_volume: 1,
        is_npc: false,
        escrow: IskCents(0),
    };
    let o2 = MarketOrderRecord {
        order_id: 2,
        price: IskCents::from_isk(9),
        ..o1.clone()
    };
    let o3 = MarketOrderRecord {
        order_id: 3,
        price: IskCents::from_isk(15),
        ..o1.clone()
    };
    let rows = best_asks_to_summary_rows([&o1, &o2, &o3].into_iter());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0].as_f64(), Some(9.0));
}

#[test]
fn structure_broker_rate_owner_notes_and_eligibility_branches() {
    let ctx = test_ctx();
    let struct_id = 1_045_000_222_i64;
    let mod_id = 1_045_000_223_i64;
    let nid = -(1_000_000_000_000_i64.saturating_add(struct_id.abs()));

    ctx.db.with_mut_snapshot(|snap| {
        // Ineligible hull type (Astrahus 35832) cannot host a market
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_144,
                name: "Astrahus".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 35_892,
                owner_id: 98_000_001,
                location_id: struct_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        assert!(!is_structure_market_online(snap, struct_id));

        // Switch to Azbel (35826), test missing quantum core and dynamic_attributes[&2] == 0.0
        snap.structures.get_mut(&struct_id).unwrap().type_id = 35826;
        snap.structures
            .get_mut(&struct_id)
            .unwrap()
            .quantum_core_installed = false;
        assert!(!is_structure_market_online(snap, struct_id));
        snap.structures
            .get_mut(&struct_id)
            .unwrap()
            .quantum_core_installed = true;
        snap.items
            .get_mut(&mod_id)
            .unwrap()
            .dynamic_attributes
            .insert(2, 0.0);
        assert!(!is_structure_market_online(snap, struct_id));
        snap.items
            .get_mut(&mod_id)
            .unwrap()
            .dynamic_attributes
            .insert(2, 1.0);
        assert!(is_structure_market_online(snap, struct_id));

        // Fallback broker share without owner_note or profile is broker_fee / 2
        assert_eq!(
            structure_owner_broker_share(snap, struct_id, IskCents(0)),
            IskCents(0)
        );
        assert_eq!(
            structure_owner_broker_share(snap, struct_id, IskCents::from_isk(1_000)),
            IskCents::from_isk(500)
        );
        assert_eq!(
            structure_owner_broker_share_with_pilot(
                snap,
                struct_id,
                IskCents::from_isk(1_000),
                None
            ),
            IskCents::from_isk(500)
        );

        // Owner note with 0.015 (1.5% owner rate -> total 2.0%, owner share = 75%)
        snap.owner_notes.insert(
            nid,
            OwnerNoteRecord {
                note_id: nid,
                character_id: -1,
                item_id: struct_id,
                label: "tax".into(),
                note: "{\"0\":0.015}".into(),
                timestamp_unix: 0,
            },
        );
        assert!((resolve_structure_broker_rate(snap, struct_id) - 0.020).abs() < 1e-9);
        assert_eq!(
            structure_owner_broker_share(snap, struct_id, IskCents::from_isk(2_000)),
            IskCents::from_isk(1_500)
        );

        // Owner note with 0.0 owner rate -> owner share is 0
        snap.owner_notes.get_mut(&nid).unwrap().note = "{\"0\":0.0}".into();
        assert_eq!(
            structure_owner_broker_share(snap, struct_id, IskCents::from_isk(2_000)),
            IskCents(0)
        );
    });
}

#[test]
fn market_proxy_error_guards_and_corporate_buy_order_matching() {
    let ctx = test_ctx();
    let buyer_id = 90_000_001_i64;
    let seller_id = 90_000_002_i64;
    let corp_id = 98_600_001_i64;
    let kw = HashMap::new();
    let mut sess = jita_session(buyer_id, corp_id);
    let call_mp = |s: &mut SessionState, method: &str, args: &[PyRep]| {
        handle(&ctx, s, "marketProxy", method, args, &kw).unwrap()
    };

    assert!(handle(&ctx, &mut sess, "config", "GetMarketGroups", &[], &kw).is_none());

    // GetMarketGroups returns market groups rowset
    let mg_groups = call_mp(&mut sess, "GetMarketGroups", &[]).unwrap();
    assert!(mg_groups.row_count().is_some());

    // Session error guards on GetOrders, GetStationAsks, GetRegionBest, PlaceBuyOrder
    assert_eq!(call_mp(&mut sess, "GetOrders", &[]).unwrap_err().code, "InvalidMarketOrder");
    sess.region_id = None;
    for m in ["GetOrders", "GetStationAsks", "GetRegionBest", "PlaceBuyOrder"] {
        assert_eq!(call_mp(&mut sess, m, &[PyRep::Integer(34)]).unwrap_err().code, "NoRegion");
    }
    sess.region_id = Some(THE_FORGE_REGION_ID);
    sess.station_id = None;
    for m in ["GetStationAsks", "PlaceBuyOrder"] {
        assert_eq!(call_mp(&mut sess, m, &[]).unwrap_err().code, "NotDockedAtStation");
    }
    sess.station_id = Some(JITA_STATION_ID);

    // PlaceBuyOrder parameter validation: missing typeID, invalid duration, invalid range, 5-sig-fig tick size, station mismatch
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "marketProxy",
            "PlaceBuyOrder",
            &[PyRep::Integer(JITA_STATION_ID)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidMarketOrder"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(34),
                PyRep::Real(10.0),
                PyRep::Integer(5),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(5), // invalid duration
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidMarketOrder"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(34),
                PyRep::Real(123.45), // 5 sig figs
                PyRep::Integer(5),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidPriceTickSize"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(60_003_761),
                PyRep::Integer(34),
                PyRep::Real(10.0),
                PyRep::Integer(5),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "StationMismatch"
    );

    // Immediate order (duration=0) when no matching sell order exists -> ImmediateOrderUnfilled
    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        if let Some(ch) = s.characters.get_mut(&buyer_id) {
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(50_000_000);
        }
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(34),
                PyRep::Real(10.0),
                PyRep::Integer(5),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(0),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ImmediateOrderUnfilled"
    );

    // Corporate buy order: CorporationNotFound -> CorpRoleRequired -> success with ROLE_TRADER
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(34),
                PyRep::Real(10.0),
                PyRep::Integer(5),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
                PyRep::Bool(true),
                PyRep::Integer(1000),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorporationNotFound"
    );

    ctx.db.with_mut_snapshot(|s| {
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "Trade Corp".into();
            __corp.ticker = "TRDC".into();
            __corp.ceo_id = seller_id;
            __corp.set_division_balance(1, IskCents::from_isk(50_000_000));
            __corp };
        corp.ensure_wallet_divisions();
        s.corporations.insert(corp_id, corp);
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(34),
                PyRep::Real(10.0),
                PyRep::Integer(5),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
                PyRep::Bool(true),
                PyRep::Integer(1000),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleRequired"
    );

    // Grant ROLE_TRADER and seed a PLEX sell order to verify immediate fill into PLEX Vault for character
    ctx.db.with_mut_snapshot(|s| {
        s.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(buyer_id, corp_roles::ROLE_TRADER);
        s.market_orders.insert(
            860_001,
            MarketOrderRecord {
                order_id: 860_001,
                owner_id: corp_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: PLEX_TYPE_ID,
                is_bid: false,
                price: IskCents::from_isk(1_000),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });
    let plex_buy = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(PLEX_TYPE_ID as i64),
            PyRep::Real(1_000.0),
            PyRep::Integer(10),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .unwrap()
    .expect("immediate PLEX buy");
    assert_eq!(
        plex_buy.dict_get("filledImmediately").and_then(|v| v.as_bool()),
        Some(true)
    );

    // Issue #622: Undocked (in space) immediate PlaceBuyOrder & BuyMultipleItems succeed,
    // while resting PlaceBuyOrder in space requires Procurement skill.
    let seller_id = 90_000_099;
    const JITA_SYSTEM_ID: u32 = 30_000_142;
    ctx.db.with_mut_snapshot(|s| {
        let mut seller = s.characters.get(&buyer_id).unwrap().clone();
        seller.character_id = seller_id;
        seller.balance = IskCents::from_isk(1_000_000);
        seller.station_id = Some(JITA_STATION_ID);
        seller.solar_system_id = JITA_SYSTEM_ID;
        s.characters.insert(seller_id, seller);
        s.market_orders.insert(
            860_002,
            MarketOrderRecord {
                order_id: 860_002,
                owner_id: seller_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(5),
                vol_entered: 20,
                vol_remaining: 20,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        if let Some(ch) = s.characters.get_mut(&buyer_id) {
            ch.balance = IskCents::from_isk(100_000);
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        s.skills.remove(&buyer_id);
    });
    let mut space_sess = SessionState::default();
    space_sess.character_id = Some(buyer_id);
    space_sess.user_id = Some(1);
    space_sess.solar_system_id = Some(JITA_SYSTEM_ID);
    space_sess.region_id = Some(THE_FORGE_REGION_ID);
    space_sess.station_id = None;

    // Immediate buy (duration=0) while in space fills from Jita sell order
    let space_imm = handle(
        &ctx,
        &mut space_sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(34),
            PyRep::Real(5.0),
            PyRep::Integer(5),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Bool(false),
        ],
        &kw,
    )
    .unwrap()
    .expect("immediate PlaceBuyOrder in space should succeed");
    assert_eq!(
        space_imm.dict_get("filledImmediately").and_then(|v| v.as_bool()),
        Some(true)
    );

    // Resting buy order in space without Procurement fails with NotDockedAtStation
    let space_rest_err = handle(
        &ctx,
        &mut space_sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(34),
            PyRep::Real(4.0),
            PyRep::Integer(5),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(space_rest_err.code, "NotDockedAtStation");

    // With Procurement >= 1, resting buy order in same solar system succeeds from space
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(buyer_id).or_default().insert(
            16594,
            eve_db::TrainedSkillRecord {
                type_id: 16594,
                level: 1,
                skill_points: 1000.0,
            },
        );
    });
    let space_rest_ok = handle(
        &ctx,
        &mut space_sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(34),
            PyRep::Real(4.0),
            PyRep::Integer(5),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
        &kw,
    )
    .unwrap()
    .expect("resting PlaceBuyOrder in space with Procurement 1 should succeed");
    assert_eq!(
        space_rest_ok.dict_get("filledImmediately").and_then(|v| v.as_bool()),
        Some(false)
    );

    // BuyMultipleItems with explicit stationID while in space succeeds
    let space_multi = handle(
        &ctx,
        &mut space_sess,
        "marketProxy",
        "BuyMultipleItems",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(5)])]),
        ],
        &kw,
    )
    .unwrap()
    .expect("BuyMultipleItems with explicit stationID in space should succeed");
    assert_eq!(
        space_multi.dict_get("totalQuantity").and_then(|v| v.as_i64()),
        Some(5)
    );

    // Direct unit checks for `notify_own_order_changed` (corp order non-member fallback + char order)
    // and `can_caller_view_upwell_market_orders`
    let buyer_uid = ctx.db.with_mut_snapshot(|snap| {
        let uid = snap.characters[&buyer_id].user_id;
        if !snap.accounts.values().any(|a| a.user_id == uid) {
            snap.accounts.insert(
                format!("mkt_buyer_{uid}"),
                eve_db::AccountRecord {
                    user_id: uid,
                    username: format!("mkt_buyer_{uid}"),
                    password_token: String::new(),
                    role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
        uid
    });
    ctx.db
        .acquire_online_lock(buyer_uid, 9301, Some(buyer_id), true, ctx.now_unix())
        .unwrap();
    let _ = ctx.drain_notifications_for_char(buyer_id);
    super::notify_own_order_changed(&ctx, buyer_id, 0, 860_002, "Add");
    assert!(ctx
        .drain_notifications_for_char(buyer_id)
        .iter()
        .any(|n| n.event == "OnOwnOrdersChanged"));

    // When `owner_id` is a corporation and `fallback_char_id` is not in `corp.members`, fallback_char_id still receives `OnOwnOrdersChanged`.
    super::notify_own_order_changed(&ctx, corp_id, buyer_id, 860_001, "Change");
    assert!(ctx
        .drain_notifications_for_char(buyer_id)
        .iter()
        .any(|n| n.event == "OnOwnOrdersChanged"));

    ctx.db.with_snapshot(|s| {
        assert!(super::can_caller_view_upwell_market_orders(
            &ctx,
            s,
            buyer_id,
            JITA_STATION_ID,
            ctx.now_unix()
        ));
    });
}

#[test]
fn issue_789_partial_self_fill_resting_buy_emits_single_final_account_change() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_seller_id = 90_000_002_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let mut sess = jita_session(char_id, corp_id);
    let kw = HashMap::new();

    // Case 1: Partial self-fill (10 units) + resting buy remainder (15 units with broker fee & escrow).
    // Seller account snapshot captured mid-loop must NOT overwrite buyer_acct_after with a stale
    // pre-broker / pre-escrow balance (#789 Bug A).
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.clear();
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(1_000_000);
        }
        snap.market_orders.insert(
            99_789_001,
            MarketOrderRecord {
                order_id: 99_789_001,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_f64_isk(100.0),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });
    ctx.db
        .acquire_online_lock(1, 9301, Some(char_id), true, ctx.now_unix())
        .unwrap();
    let other_uid = ctx.db.with_mut_snapshot(|snap| {
        let uid = snap.characters[&other_seller_id].user_id;
        if !snap.accounts.values().any(|a| a.user_id == uid) {
            snap.accounts.insert(
                format!("mkt_seller_{uid}"),
                eve_db::AccountRecord {
                    user_id: uid,
                    username: format!("mkt_seller_{uid}"),
                    password_token: String::new(),
                    role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
        uid
    });
    ctx.db
        .acquire_online_lock(other_uid, 9302, Some(other_seller_id), true, ctx.now_unix())
        .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);

    let partial_rep = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(34),
            PyRep::Real(100.0),
            PyRep::Integer(25),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
        &kw,
    )
    .unwrap()
    .expect("partial self-fill + resting buy remainder succeeds");
    assert_eq!(
        partial_rep.dict_get("filledImmediately").and_then(PyRep::as_bool),
        Some(false)
    );
    let final_bal_1 = ctx.db.get_character(char_id).unwrap().balance.to_wire_f64();
    let acct_bals_1: Vec<f64> = ctx
        .drain_notifications_for_char(char_id)
        .into_iter()
        .filter(|n| n.event == "OnAccountChange")
        .filter_map(|n| n.args.get(2).and_then(PyRep::as_f64))
        .collect();
    assert_eq!(
        acct_bals_1.len(),
        1,
        "Partial self-fill + resting buy must emit single final OnAccountChange without stale pre-escrow overwrite: {acct_bals_1:?}"
    );
    assert!(
        (acct_bals_1[0] - final_bal_1).abs() < 1e-2,
        "OnAccountChange balance {} must equal final post-escrow balance {final_bal_1}",
        acct_bals_1[0]
    );

    // Case 2: Immediate multi-fill where fill #1 is a self-fill and fill #2 buys from another seller.
    // Fill #1's stale post-self-fill balance must NOT overwrite fill #2's final buyer balance.
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.clear();
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(1_000_000);
        }
        snap.market_orders.insert(
            99_789_002,
            MarketOrderRecord {
                order_id: 99_789_002,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_f64_isk(50.0),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            99_789_003,
            MarketOrderRecord {
                order_id: 99_789_003,
                owner_id: other_seller_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_f64_isk(60.0),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = ctx.drain_notifications_for_char(other_seller_id);

    let multi_rep = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(34),
            PyRep::Real(60.0),
            PyRep::Integer(20),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Bool(false),
        ],
        &kw,
    )
    .unwrap()
    .expect("immediate self-fill + third-party fill succeeds");
    assert_eq!(
        multi_rep.dict_get("filledImmediately").and_then(PyRep::as_bool),
        Some(true)
    );
    let final_bal_2 = ctx.db.get_character(char_id).unwrap().balance.to_wire_f64();
    let acct_bals_2: Vec<f64> = ctx
        .drain_notifications_for_char(char_id)
        .into_iter()
        .filter(|n| n.event == "OnAccountChange")
        .filter_map(|n| n.args.get(2).and_then(PyRep::as_f64))
        .collect();
    assert_eq!(
        acct_bals_2.len(),
        1,
        "Multi-fill starting with self-fill must emit single final OnAccountChange: {acct_bals_2:?}"
    );
    assert!(
        (acct_bals_2[0] - final_bal_2).abs() < 1e-2,
        "OnAccountChange balance {} must equal final balance {final_bal_2}",
        acct_bals_2[0]
    );
    let other_bal = ctx.db.get_character(other_seller_id).unwrap().balance.to_wire_f64();
    let other_bals: Vec<f64> = ctx
        .drain_notifications_for_char(other_seller_id)
        .into_iter()
        .filter(|n| n.event == "OnAccountChange")
        .filter_map(|n| n.args.get(2).and_then(PyRep::as_f64))
        .collect();
    assert_eq!(other_bals.len(), 1);
    assert!((other_bals[0] - other_bal).abs() < 1e-2);
}
