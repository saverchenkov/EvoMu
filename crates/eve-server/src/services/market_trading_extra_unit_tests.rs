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
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "Trade Corp".into(),
            ticker: "TRDC".into(),
            ceo_id: seller_id,
            master_wallet: IskCents::from_isk(50_000_000),
            ..Default::default()
        };
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
}
