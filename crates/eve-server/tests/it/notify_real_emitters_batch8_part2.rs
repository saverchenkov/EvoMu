//! Real server-side emission & RPC tests for Notify/RPC Batch 8 (Part 2: HyperNet Raffles & PLEX Vault).
//!
//! Covers 7 Group 2 HyperNet Raffles & PLEX Vault parity rows:
//! - `rpc:raffleMgr.QA_SeedRaffles`
//! - `notify:OnRaffleCreatedServer`
//! - `notify:OnRaffleFinishedServer`
//! - `notify:OnRaffleUpdatedServer`
//! - `notify:OnTicketsUpdatedServer`
//! - `notify:OnPlexDeposited`
//! - `notify:OnPlexItemAddedToInventory`

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::hypernet_raffle_ops::{sweep_expired_raffles, HYPERCORE_TYPE_ID};
use eve_server::services::market_contracts::PLEX_TYPE_ID;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const JITA_STATION: i64 = 60_003_760;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx.set_mechanic_rng_seed(Some(77));
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(e, _)| e == event)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| {
            let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
            panic!("expected notification {event}, got {seen:?}")
        })
}

async fn login_docked(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(JITA_STATION);
            ch.structure_id = None;
            ch.balance = IskCents::from_f64_isk(500_000_000.0);
        }
    });
    sess.station_id = Some(JITA_STATION);
    sess.structure_id = None;
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

// parity: rpc:raffleMgr.QA_SeedRaffles, notify:OnRaffleCreatedServer, notify:OnRaffleFinishedServer
// parity: notify:OnRaffleUpdatedServer, notify:OnTicketsUpdatedServer, notify:OnPlexDeposited
// parity: notify:OnPlexItemAddedToInventory
#[tokio::test]
async fn notify_batch8_hypernet_raffles_and_plex_vault() {
    eve_testkit::attest(&[
        "rpc:raffleMgr.QA_SeedRaffles",
        "notify:OnRaffleCreatedServer",
        "notify:OnRaffleFinishedServer",
        "notify:OnRaffleUpdatedServer",
        "notify:OnTicketsUpdatedServer",
        "notify:OnPlexDeposited",
        "notify:OnPlexItemAddedToInventory",
    ]);

    let ctx = new_ctx();
    let mut unauth = SessionState::new(9_200);
    assert_eq!(
        rpc(&ctx, &mut unauth, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(2)])
            .await
            .unwrap_err()
            .code,
        "Unauthorized"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut unauth,
            "invbroker",
            "DepositPlexToVault",
            &[PyRep::Integer(JITA_STATION), PyRep::Integer(1), PyRep::Integer(10)],
        )
        .await
        .unwrap_err()
        .code,
        "Unauthorized"
    );

    let mut sess_a = login_docked(&ctx, 9_201, "demo", "demo-secret", PILOT_A).await;
    let mut sess_b = login_docked(&ctx, 9_202, "other", "other-secret", PILOT_B).await;

    // 1. QA_SeedRaffles role check + positive seeding
    assert_eq!(
        rpc(&ctx, &mut sess_a, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(3)])
            .await
            .unwrap_err()
            .code,
        "InsufficientRole"
    );
    sess_a.role |= ROLE_GM_DEFAULT;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let seeded_rep = rpc(&ctx, &mut sess_a, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(3)])
        .await
        .expect("QA_SeedRaffles");
    let PyRep::List(seeded_list) = seeded_rep else {
        panic!("expected List from QA_SeedRaffles, got {seeded_rep:?}");
    };
    assert_eq!(seeded_list.len(), 3);
    let seeded_rid_0 = seeded_list[0].dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let seeded_iid_0 = seeded_list[0].dict_get("item_id").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(seeded_rid_0 > 0 && seeded_iid_0 > 0);
    assert!(seeded_list[0].dict_get("sold_tickets").is_some(), "QA_SeedRaffles must include sold_tickets");
    let seeded_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let created_rids: std::collections::BTreeSet<i64> = seeded_notifs
        .iter()
        .filter(|(n, _)| n == "OnRaffleCreatedServer")
        .filter_map(|(_, a)| a.first().and_then(|v| v.as_i64()))
        .collect();
    assert_eq!(
        created_rids.len(),
        3,
        "QA_SeedRaffles(3) must emit OnRaffleCreatedServer for all 3 seeded raffles"
    );
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&seeded_iid_0).expect("seeded item in snap.items");
        assert_eq!((it.owner_id, it.location_id, it.flag), (-52_568, -52_568, InventoryFlag::Locked));
    });
    let _ = rpc(&ctx, &mut sess_b, "raffleProxy", "BuyRandomTickets", &[PyRep::Integer(seeded_rid_0), PyRep::Integer(8)])
        .await
        .expect("BuyRandomTickets on seeded raffle");
    let claimed_seeded = rpc(&ctx, &mut sess_b, "raffleProxy", "AwardItem", &[PyRep::Integer(seeded_rid_0)])
        .await
        .expect("AwardItem on seeded raffle");
    assert_eq!(claimed_seeded.as_i64(), Some(seeded_iid_0));
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&seeded_iid_0).expect("awarded seeded item");
        assert_eq!((it.owner_id, it.location_id, it.flag), (PILOT_B, JITA_STATION, InventoryFlag::Hangar));
    });
    let seeded_tc = seeded_list[0].dict_get("ticket_count").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(
        [8, 16, 48, 512].contains(&seeded_tc),
        "QA_SeedRaffles ticket_count must be in [8, 16, 48, 512], got {seeded_tc}"
    );
    assert_eq!(seeded_list[0].dict_get("owner_id").and_then(|v| v.as_i64()), Some(PILOT_A));
    assert_eq!(seeded_list[0].dict_get("location_id").and_then(|v| v.as_i64()), Some(JITA_STATION));

    // Undocked GM session must fail closed (no Jita fallback)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = None;
        }
    });
    let mut undocked_gm = sess_a.clone();
    undocked_gm.station_id = None;
    assert!(
        rpc(&ctx, &mut undocked_gm, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(1)])
            .await
            .is_err(),
        "QA_SeedRaffles must fail closed when station_id is None"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(JITA_STATION);
        }
    });

    // 2. CreateRaffle -> emits OnRaffleCreatedServer(raffle_id, raffle_kv)
    let (item_1, item_2, cores_id) = (820_000_001_i64, 820_000_002_i64, 820_000_003_i64);
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, qty, sing) in [
            (item_1, 587u32, 1, true),
            (item_2, 587u32, 1, true),
            (cores_id, HYPERCORE_TYPE_ID, 50, false),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: PILOT_A,
                    location_id: JITA_STATION,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: sing,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    // Negative: invalid ticket count (7 not in [8, 16, 48, 512]) and missing item rejected
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::Integer(item_1), PyRep::Integer(7), PyRep::Real(100_000.0)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidRaffleTicketCount"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::Integer(999_888_777), PyRep::Integer(8), PyRep::Real(100_000.0)],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFound"
    );

    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let create_rep = rpc(
        &ctx,
        &mut sess_a,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Integer(item_1), PyRep::Integer(8), PyRep::Real(100_000.0)],
    )
    .await
    .expect("CreateRaffle");
    let raffle_id = create_rep
        .dict_get("raffle_id")
        .and_then(|v| v.as_i64())
        .expect("raffle_id");
    let create_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let created_args = find_notif(&create_notifs, "OnRaffleCreatedServer");
    assert_eq!(created_args.len(), 2);
    assert_eq!(created_args[0].as_i64(), Some(raffle_id));
    assert_eq!(
        created_args[1].dict_get("ticket_count").and_then(|v| v.as_i64()),
        Some(8)
    );
    // Calling QA_SeedRaffles after CreateRaffle must not collide with or overwrite raffle_id
    let post_seed_rep = rpc(&ctx, &mut sess_a, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(1)])
        .await
        .expect("QA_SeedRaffles after CreateRaffle");
    let PyRep::List(post_seed_list) = post_seed_rep else {
        panic!("expected List from QA_SeedRaffles");
    };
    let post_seed_rid = post_seed_list[0].dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap_or(0);
    assert_ne!(post_seed_rid, raffle_id, "QA_SeedRaffles must not collide with CreateRaffle raffle_id");
    let live_raffle = rpc(&ctx, &mut sess_a, "raffleProxy", "GetRaffle", &[PyRep::Integer(raffle_id)])
        .await
        .expect("GetRaffle live");
    assert_eq!(live_raffle.dict_get("item_id").and_then(|v| v.as_i64()), Some(item_1));

    // 3. BuyTicket & BuyRandomTickets -> emits OnRaffleUpdatedServer & OnTicketsUpdatedServer,
    // and buying the final ticket emits OnRaffleFinishedServer(raffle_id, winning_ticket)
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    let _ = rpc(
        &ctx,
        &mut sess_b,
        "raffleProxy",
        "BuyTicket",
        &[PyRep::Integer(raffle_id), PyRep::Integer(0)],
    )
    .await
    .expect("BuyTicket 0");

    // Negative: buying already-sold ticket 0 fails with TicketUnavailable
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_b,
            "raffleProxy",
            "BuyTicket",
            &[PyRep::Integer(raffle_id), PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "TicketUnavailable"
    );

    let upd_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    let upd_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let upd_args = find_notif(&upd_notifs_b, "OnRaffleUpdatedServer");
    assert_eq!(upd_args.len(), 2);
    assert_eq!(upd_args[0].as_i64(), Some(raffle_id));
    assert_eq!(
        upd_args[1].dict_get("tickets_remaining").and_then(|v| v.as_i64()),
        Some(7)
    );
    let tix_args = find_notif(&upd_notifs_a, "OnTicketsUpdatedServer");
    assert_eq!(tix_args.len(), 1);
    assert_eq!(
        tix_args[0],
        PyRep::Dict(vec![(PyRep::Integer(raffle_id), PyRep::Integer(1))])
    );

    // Buy remaining 7 tickets -> finishes raffle and emits OnRaffleFinishedServer
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    let _ = rpc(
        &ctx,
        &mut sess_b,
        "raffleProxy",
        "BuyRandomTickets",
        &[PyRep::Integer(raffle_id), PyRep::Integer(7)],
    )
    .await
    .expect("BuyRandomTickets 7");

    let fin_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let fin_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    let fin_args_a = find_notif(&fin_notifs_a, "OnRaffleFinishedServer");
    let fin_args_b = find_notif(&fin_notifs_b, "OnRaffleFinishedServer");
    assert_eq!(fin_args_a.len(), 2);
    assert_eq!(fin_args_a[0].as_i64(), Some(raffle_id));
    assert!(fin_args_a[1].as_i64().is_some_and(|t| (0..8).contains(&t)));
    assert_eq!(fin_args_a, fin_args_b);

    // Negative: buying on finished raffle fails with RaffleFinished; also verify expiry sweep emits None winner
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_b,
            "raffleProxy",
            "BuyRandomTickets",
            &[PyRep::Integer(raffle_id), PyRep::Integer(1)],
        )
        .await
        .unwrap_err()
        .code,
        "RaffleFinished"
    );

    let create_2 = rpc(
        &ctx,
        &mut sess_a,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Integer(item_2), PyRep::Integer(8), PyRep::Real(100_000.0), PyRep::Integer(60)],
    )
    .await
    .expect("CreateRaffle 2");
    let raffle_2 = create_2.dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap();
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    ctx.advance_secs(120);
    assert_eq!(sweep_expired_raffles(&ctx), 1);
    let exp_raffle_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&exp_raffle_notifs, "OnRaffleFinishedServer"),
        &[PyRep::Integer(raffle_2), PyRep::None]
    );

    // 4. PLEX Vault deposit & withdraw: positive + negative failure notifications
    let (plex_item_id, tritanium_id) = (820_000_010_i64, 820_000_011_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            plex_item_id,
            InventoryItemRecord {
                item_id: plex_item_id,
                type_id: PLEX_TYPE_ID,
                owner_id: PILOT_A,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            tritanium_id,
            InventoryItemRecord {
                item_id: tritanium_id,
                type_id: 34,
                owner_id: PILOT_A,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Negative deposit cases -> synchronous RPC Err without unmatchable OnPlexDepositFailed
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "invbroker",
            "DepositPlexToVault",
            &[PyRep::Integer(JITA_STATION), PyRep::Integer(tritanium_id), PyRep::Integer(10)],
        )
        .await
        .unwrap_err()
        .code,
        "NotPlex"
    );
    let dep_fail_1 = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!dep_fail_1.iter().any(|(e, _)| e == "OnPlexDepositFailed"));

    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "invbroker",
            "DepositPlexToVault",
            &[PyRep::Integer(JITA_STATION), PyRep::Integer(plex_item_id), PyRep::Integer(9_999)],
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughQuantity"
    );
    let dep_fail_2 = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!dep_fail_2.iter().any(|(e, _)| e == "OnPlexDepositFailed"));

    // Positive deposit -> emits OnPlexDeposited([entity_id])
    let dep_ok = rpc(
        &ctx,
        &mut sess_a,
        "invbroker",
        "DepositPlexToVault",
        &[PyRep::Integer(JITA_STATION), PyRep::Integer(plex_item_id), PyRep::Integer(250)],
    )
    .await
    .expect("DepositPlexToVault 250");
    assert!(dep_ok.as_i64().unwrap_or(0) > 0);
    let dep_ok_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let dep_args = find_notif(&dep_ok_notifs, "OnPlexDeposited");
    assert_eq!(dep_args.len(), 1);
    assert_eq!(dep_args[0], dep_ok);

    // Negative withdraw cases -> synchronous RPC Err without unmatchable OnPlexWithdrawFailed
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "invbroker",
            "WithdrawPlexFromVault",
            &[PyRep::Integer(99_999)],
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughPlex"
    );
    let wd_fail_1 = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!wd_fail_1.iter().any(|(e, _)| e == "OnPlexWithdrawFailed"));

    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "invbroker",
            "WithdrawPlexFromVault",
            &[PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    let wd_fail_2 = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!wd_fail_2.iter().any(|(e, _)| e == "OnPlexWithdrawFailed"));

    // Positive withdraw -> emits OnPlexItemAddedToInventory([item_id, tx_id]) matching reply reference
    let wd_ok = rpc(
        &ctx,
        &mut sess_a,
        "invbroker",
        "WithdrawPlexFromVault",
        &[PyRep::Integer(100)],
    )
    .await
    .expect("WithdrawPlexFromVault 100");
    let withdrawn_item_id = wd_ok
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("withdrawn itemID");
    let wd_ref = wd_ok
        .dict_get("reference")
        .and_then(|v| v.as_i64())
        .expect("withdraw reference");
    assert_eq!(
        wd_ok.dict_get("remainingVaultBalance").and_then(|v| v.as_i64()),
        Some(650)
    );
    let wd_ok_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let wd_args = find_notif(&wd_ok_notifs, "OnPlexItemAddedToInventory");
    assert_eq!(wd_args.len(), 2);
    assert_eq!(wd_args[0].as_i64(), Some(withdrawn_item_id));
    assert_eq!(wd_args[1].as_i64(), Some(wd_ref));
}
