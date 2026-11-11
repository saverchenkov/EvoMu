//! Edge-case behavioral tests for `hypernet_raffle_ops`.

use super::*;
use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord, VirtualServerClock,
};
use eve_machonet::service_roles;
use eve_sde::InvType;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

fn add_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton: quantity == 1,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

#[test]
fn sweep_expired_raffles_refunds_tickets_returns_escrow_and_strips_cosmetics() {
    let ctx = test_ctx();
    let base = 1_750_000_000;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base)));
    let creator = 90_000_001;
    let buyer1 = 90_000_002;
    let buyer2 = 90_000_003;
    let station_id = 60_003_760;

    ctx.db.with_mut_snapshot(|snap| {
        let mut b1 = snap.characters[&creator].clone();
        b1.character_id = buyer1;
        b1.balance = IskCents::from_isk(100);
        snap.characters.insert(buyer1, b1);

        let mut b2 = snap.characters[&creator].clone();
        b2.character_id = buyer2;
        b2.balance = IskCents::from_isk(200);
        snap.characters.insert(buyer2, b2);

        // Escrowed ship owned by someone else with an applied skin
        snap.items.insert(
            880_001,
            InventoryItemRecord {
                item_id: 880_001,
                type_id: 587,
                owner_id: buyer1,
                location_id: 999,
                flag: InventoryFlag::Locked,
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
        snap.applied_ship_skins.insert(880_001, 42);

        // Expired raffle with 3 sold tickets (2 to buyer1, 1 to buyer2)
        let expired = RaffleStateRecord {
            raffle_id: 1,
            creator_id: creator,
            station_id,
            item_id: 880_001,
            type_id: 587,
            ticket_count: 8,
            ticket_price_cents: 500, // 5.00 ISK per ticket
            hypercores_used: 1,
            created_unix: base - 10_000,
            expires_unix: base - 10,
            sold_tickets: BTreeMap::from([(0, buyer1), (1, buyer2), (2, buyer1)]),
            winning_ticket: None,
            winner_id: None,
            item_awarded: false,
            expired: false,
        };
        save_json_note(
            snap,
            raffle_note_id(1),
            -74,
            1,
            "raffle:1".into(),
            &expired,
            base - 10_000,
        );

        // Future raffle (not yet expired) must NOT be swept
        let future = RaffleStateRecord {
            raffle_id: 2,
            creator_id: creator,
            station_id,
            item_id: 880_002,
            type_id: 587,
            ticket_count: 8,
            ticket_price_cents: 500,
            hypercores_used: 1,
            created_unix: base,
            expires_unix: base + 3600,
            sold_tickets: BTreeMap::from([(0, buyer1)]),
            winning_ticket: None,
            winner_id: None,
            item_awarded: false,
            expired: false,
        };
        save_json_note(snap, raffle_note_id(2), -74, 2, "raffle:2".into(), &future, base);
    });

    let creator_uid = ctx.db.with_snapshot(|s| s.characters[&creator].user_id);
    ctx.db
        .acquire_online_lock(creator_uid, 9_003, Some(creator), true, base)
        .unwrap();
    ctx.pending_notifications.write().unwrap().clear();
    let swept = sweep_expired_raffles(&ctx);
    assert_eq!(swept, 1);

    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.characters[&buyer1].balance,
            IskCents(IskCents::from_isk(100).0 + 1_000)
        );
        assert_eq!(
            snap.characters[&buyer2].balance,
            IskCents(IskCents::from_isk(200).0 + 500)
        );
        let item = &snap.items[&880_001];
        assert_eq!(item.owner_id, creator);
        assert_eq!(item.location_id, station_id);
        assert_eq!(item.flag, InventoryFlag::Hangar);
        assert!(!snap.applied_ship_skins.contains_key(&880_001));

        let r1: RaffleStateRecord =
            serde_json::from_str(&snap.owner_notes[&raffle_note_id(1)].note).unwrap();
        assert!(r1.expired);
        let r2: RaffleStateRecord =
            serde_json::from_str(&snap.owner_notes[&raffle_note_id(2)].note).unwrap();
        assert!(!r2.expired);
    });

    let events: Vec<String> = ctx.pending_notifications.read().unwrap()[&creator]
        .iter()
        .map(|n| n.event.clone())
        .collect();
    assert!(events.contains(&"OnRaffleFinishedServer".to_string()));
}

#[test]
fn subscription_eviction_and_active_historic_price_fallbacks() {
    let mut ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // Fill 256 subscription keys; inserting a 257th triggers eviction back under cap
    for k in 1..=257_i64 {
        let res = handle(
            &ctx,
            &mut sess,
            "raffleProxy",
            "SubscribeToTickets",
            &[PyRep::Integer(k)],
            &kw,
        )
        .unwrap()
        .unwrap();
        assert_eq!(res, PyRep::None);
    }
    ctx.db.with_snapshot(|snap| {
        let subs = &snap.fleet_and_social_ops.hypernet_subs;
        assert!(subs.len() <= 256, "len={}", subs.len());
    });

    // Unsubscribe cleans empty keys cleanly
    assert!(handle(
        &ctx,
        &mut sess,
        "raffleProxy",
        "UnSubscribeFromTickets",
        &[PyRep::Integer(257)],
        &kw
    )
    .unwrap()
    .is_ok());

    // GetActiveHistoricPrices invalid / <= 0 type_id errors vs market_orders sell fallback
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "raffleProxy",
            "GetActiveHistoricPrices",
            &[PyRep::Integer(0)],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    let tid = 91_234_u32;
    let mut sde_mut = (*ctx.sde).clone();
    sde_mut.types.insert(
        tid,
        InvType {
            type_id: tid,
            group_id: 25,
            category_id: 6,
            name: "PriceTestShip".into(),
            mass: 1000.0,
            volume: 100.0,
            capacity: 10.0,
            radius: 10.0,
            base_price: 2_000_000.0,
            published: true,
            portion_size: 1,
            market_group_id: None,
            race_id: None,
            faction_id: None,
            variation_parent_type_id: None,
            description_id: None,
            icon_id: None,
            graphic_id: None,
            sound_id: None,
            meta_group_id: None,
            meta_level: None,
            tech_level: None,
            default_effect_id: None,
            description: String::new(),
            effect_ids: Vec::new(),
            default_attributes: HashMap::new(),
        },
    );
    ctx.sde = Arc::new(sde_mut);
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            700_001,
            MarketOrderRecord {
                order_id: 700_001,
                owner_id: 90_000_001,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: tid,
                is_bid: false,
                price: IskCents::from_isk(1_500_000),
                vol_entered: 2,
                vol_remaining: 2,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });
    let prices = handle(
        &ctx,
        &mut sess,
        "raffleProxy",
        "GetActiveHistoricPrices",
        &[PyRep::Integer(tid as i64)],
        &kw,
    )
    .unwrap()
    .unwrap();
    match prices {
        PyRep::Tuple(parts) => {
            assert_eq!(parts.len(), 2);
            assert_eq!(parts[0], PyRep::Real(1_500_000.0));
            assert_eq!(parts[1], PyRep::Real(2_000_000.0));
        }
        other => panic!("expected 2-tuple of prices, got {other:?}"),
    }
}

#[test]
fn create_raffle_location_item_hypercore_validation_and_buy_ticket_award_edges() {
    let ctx = test_ctx();
    let base = 1_750_000_000;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base)));
    let creator = 90_000_001;
    let buyer = 90_000_002;
    let station_id = 60_003_760;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut b = snap.characters[&creator].clone();
        b.character_id = buyer;
        b.balance = IskCents::from_isk(50_000_000);
        snap.characters.insert(buyer, b);
    });

    let mut creator_sess = auth_session(1, creator, 98_000_001, 30_000_142);
    creator_sess.station_id = Some(station_id);

    // Undocked -> InvalidRaffleLocation
    let mut undocked = creator_sess.clone();
    undocked.station_id = None;
    let res_undocked = handle(
        &ctx,
        &mut undocked,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Integer(1), PyRep::Integer(8), PyRep::Real(1000.0)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(res_undocked.code, "InvalidRaffleLocation");

    // Structure docked -> InvalidRaffleLocation
    let mut struct_sess = creator_sess.clone();
    struct_sess.structure_id = Some(1_000_000_001);
    let res_struct = handle(
        &ctx,
        &mut struct_sess,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Integer(1), PyRep::Integer(8), PyRep::Real(1000.0)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(res_struct.code, "InvalidRaffleLocation");

    // Non-singleton stacked item, active ship, and non-empty container are rejected with InvalidRaffleItem
    add_item(&ctx, 881_001, 587, creator, station_id, InventoryFlag::Hangar, 2);
    add_item(&ctx, 881_002, 587, creator, station_id, InventoryFlag::Hangar, 1);
    add_item(&ctx, 881_003, 34, creator, 881_002, InventoryFlag::Cargo, 5);
    add_item(&ctx, 881_004, 587, creator, station_id, InventoryFlag::Hangar, 1);
    let active_ship = ctx.db.with_snapshot(|s| s.characters[&creator].active_ship_id);
    creator_sess.ship_id = Some(active_ship);

    for bad_item in [881_001_i64, 881_002, active_ship] {
        let e = handle(
            &ctx,
            &mut creator_sess,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::Integer(bad_item), PyRep::Integer(8), PyRep::Real(1000.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(e.code, "InvalidRaffleItem", "item {bad_item} failed with {e:?}");
    }

    // Missing HyperCores -> InsufficientHyperCores
    let e_hc = handle(
        &ctx,
        &mut creator_sess,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Integer(881_004), PyRep::Integer(8), PyRep::Real(1000.0)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(e_hc.code, "InsufficientHyperCores");

    // Give 3 HyperCores -> excess duration (> 259200s) rejected, then valid create deducts 1 leaving 2 remaining
    add_item(
        &ctx,
        881_099,
        HYPERCORE_TYPE_ID,
        creator,
        station_id,
        InventoryFlag::Hangar,
        3,
    );
    let err_dur = handle(
        &ctx,
        &mut creator_sess,
        "raffleProxy",
        "CreateRaffle",
        &[
            PyRep::Integer(881_004),
            PyRep::Integer(8),
            PyRep::Real(1000.0),
            PyRep::Integer(300_000),
        ],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_dur.code, "InvalidRaffleDuration");
    let created = handle(
        &ctx,
        &mut creator_sess,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Integer(881_004), PyRep::Integer(8), PyRep::Real(100_000.0)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let raffle_id = created.dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&881_099].quantity), 2);

    // BuyTicket negative index / >= ticket_count -> TicketUnavailable; BuyRandomTickets invalid count -> InvalidArgument / NotEnoughTickets
    let mut buyer_sess = auth_session(2, buyer, 98_000_001, 30_000_142);
    buyer_sess.station_id = Some(station_id);
    assert_eq!(
        handle(
            &ctx,
            &mut buyer_sess,
            "raffleProxy",
            "BuyTicket",
            &[PyRep::Integer(raffle_id), PyRep::Integer(-1)],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "TicketUnavailable"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut buyer_sess,
            "raffleProxy",
            "BuyTicket",
            &[PyRep::Integer(raffle_id), PyRep::Integer(8)],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "TicketUnavailable"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut buyer_sess,
            "raffleProxy",
            "BuyRandomTickets",
            &[PyRep::Integer(raffle_id), PyRep::Integer(0)],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut buyer_sess,
            "raffleProxy",
            "BuyRandomTickets",
            &[PyRep::Integer(raffle_id), PyRep::Integer(9)],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotEnoughTickets"
    );

    // GetMyActiveTickets shows active raffle after buying 1 ticket, then disappears once finished while GetMyRaffleHistory retains it
    assert!(handle(
        &ctx,
        &mut buyer_sess,
        "raffleProxy",
        "BuyTicket",
        &[PyRep::Integer(raffle_id), PyRep::Integer(0)],
        &kw
    )
    .unwrap()
    .is_ok());

    let active_list = handle(
        &ctx,
        &mut buyer_sess,
        "raffleProxy",
        "GetMyActiveTickets",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(active_list.as_list().unwrap().len(), 1);

    // Non-winner cannot AwardItem yet (still unfinished -> NotRaffleWinner)
    assert_eq!(
        handle(
            &ctx,
            &mut creator_sess,
            "raffleProxy",
            "AwardItem",
            &[PyRep::Integer(raffle_id)],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotRaffleWinner"
    );

    // Creator balance before finishing vs finishing all remaining 7 tickets by buyer -> 5% tax deducted from creator payout!
    let creator_bal_before = ctx.db.with_snapshot(|s| s.characters[&creator].balance.0);
    assert!(handle(
        &ctx,
        &mut buyer_sess,
        "raffleProxy",
        "BuyRandomTickets",
        &[PyRep::Integer(raffle_id), PyRep::Integer(7)],
        &kw
    )
    .unwrap()
    .is_ok());
    let creator_bal_after = ctx.db.with_snapshot(|s| s.characters[&creator].balance.0);
    // 8 tickets * 100_000 ISK = 800_000 ISK -> 95% = 760_000 ISK = 76_000_000 cents
    assert_eq!(
        creator_bal_after - creator_bal_before,
        IskCents::from_isk(760_000).0
    );

    let active_after = handle(
        &ctx,
        &mut buyer_sess,
        "raffleProxy",
        "GetMyActiveTickets",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(active_after.as_list().unwrap().is_empty());

    let hist_after = handle(
        &ctx,
        &mut buyer_sess,
        "raffleProxy",
        "GetMyRaffleHistory",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(hist_after.as_list().unwrap().len(), 1);

    // Creator calls AwardItem -> NotRaffleWinner; buyer (sole buyer = winner) calls AwardItem -> succeeds & transfers item
    assert_eq!(
        handle(
            &ctx,
            &mut creator_sess,
            "raffleProxy",
            "AwardItem",
            &[PyRep::Integer(raffle_id)],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotRaffleWinner"
    );
    assert!(handle(
        &ctx,
        &mut buyer_sess,
        "raffleProxy",
        "AwardItem",
        &[PyRep::Integer(raffle_id)],
        &kw
    )
    .unwrap()
    .is_ok());
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&881_004].owner_id, buyer);
        assert_eq!(s.items[&881_004].flag, InventoryFlag::Hangar);
    });
}

#[test]
fn filtered_grab_blueprint_types_and_qa_seeding_validation_edges() {
    let ctx = test_ctx();
    let creator = 90_000_001;
    let station_id = 60_003_760;
    let kw = HashMap::new();

    // Seed an active BPO raffle and an active BPC raffle (category 9 = Blueprint in SDE, or item.is_blueprint_copy)
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, rid, is_copy, price) in [(882_001_i64, 10_i64, false, 1_000_i64), (882_002, 11, true, 5_000)] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 681,
                    owner_id: creator,
                    location_id: rid,
                    flag: InventoryFlag::Locked,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: is_copy,
                    blueprint_runs_remaining: if is_copy { Some(5) } else { None },
                },
            );
            let rec = RaffleStateRecord {
                raffle_id: rid,
                creator_id: creator,
                station_id,
                item_id: iid,
                type_id: 681,
                ticket_count: 8,
                ticket_price_cents: price * 100,
                hypercores_used: 1,
                created_unix: 1_700_000_000,
                expires_unix: 2_000_000_000,
                sold_tickets: BTreeMap::new(),
                winning_ticket: None,
                winner_id: None,
                item_awarded: false,
                expired: false,
            };
            save_json_note(snap, raffle_note_id(rid), -74, rid, format!("raffle:{rid}"), &rec, 1_700_000_000);
        }
    });

    let mut player = auth_session(1, creator, 98_000_001, 30_000_142);
    player.station_id = Some(station_id);
    player.role = service_roles::ROLE_PLAYER;

    // FilteredGrab with blueprintType=1 (BPO only) vs 2 (BPC only)
    let f_bpo = PyRep::Dict(vec![(PyRep::String("blueprintType".into()), PyRep::Integer(1))]);
    let bpo_res = handle(&ctx, &mut player, "raffleProxy", "FilteredGrab", &[f_bpo], &kw)
        .unwrap()
        .unwrap();
    let bpo_ids: Vec<i64> = bpo_res
        .as_list()
        .unwrap()
        .iter()
        .filter_map(|kv| kv.dict_get("raffle_id").and_then(|v| v.as_i64()))
        .collect();
    assert_eq!(bpo_ids, vec![10]);

    let f_bpc = PyRep::Dict(vec![(PyRep::String("blueprintType".into()), PyRep::Integer(2))]);
    let bpc_res = handle(&ctx, &mut player, "raffleProxy", "FilteredGrab", &[f_bpc], &kw)
        .unwrap()
        .unwrap();
    let bpc_ids: Vec<i64> = bpc_res
        .as_list()
        .unwrap()
        .iter()
        .filter_map(|kv| kv.dict_get("raffle_id").and_then(|v| v.as_i64()))
        .collect();
    assert_eq!(bpc_ids, vec![11]);

    // handle_qa_seed_raffles rejects non-GM/QA and undocked sessions
    assert_eq!(
        handle_qa_seed_raffles(&ctx, &mut player, &[PyRep::Integer(1)])
            .unwrap_err()
            .message,
        "NotAuthorized"
    );
    let mut qa = player.clone();
    qa.role = service_roles::ROLE_PLAYER | service_roles::ROLE_QA;
    assert!(handle_qa_seed_raffles(&ctx, &mut qa, &[PyRep::Integer(1)]).is_ok());

    // handle_character_seeding: non-privileged seeding other char -> NotAuthorized; duplicate item -> InvalidArguments; non-positive count -> InvalidArguments
    assert_eq!(
        handle_character_seeding(
            &ctx,
            &mut player,
            &[PyRep::Integer(90_000_002), PyRep::List(vec![PyRep::Integer(1)])],
            &kw,
        )
        .unwrap_err()
        .message,
        "NotAuthorized"
    );
    assert_eq!(
        handle_character_seeding(
            &ctx,
            &mut player,
            &[PyRep::List(vec![PyRep::Integer(5), PyRep::Integer(5)])],
            &kw,
        )
        .unwrap_err()
        .code,
        "InvalidArguments"
    );
    assert_eq!(
        handle_character_seeding(&ctx, &mut qa, &[PyRep::Integer(creator), PyRep::Integer(0)], &kw)
            .unwrap_err()
            .code,
        "InvalidArguments"
    );
    assert_eq!(
        handle_character_seeding(
            &ctx,
            &mut qa,
            &[
                PyRep::Integer(creator),
                PyRep::Integer(1),
                PyRep::Integer(999_999_999)
            ],
            &kw
        )
        .unwrap_err()
        .code,
        "InvalidTypeID"
    );
}
