//! Live client-RPC notification closure tests (`notify:On*`).
//!
//! Verifies server-to-client V24 notifications emitted by real domain RPCs
//! (`raffleProxy.CreateRaffle`, `raffleProxy.BuyRandomTickets`).

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const PILOT_C: i64 = 90_000_003;
const JITA_STATION: i64 = 60_003_760;

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        ..Default::default()
    }
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

async fn login_in_station(
    ctx: &ServerContext,
    conn_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(conn_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(JITA_STATION);
            ch.balance = IskCents::from_isk(100_000_000);
        }
    });
    sess.station_id = Some(JITA_STATION);
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], name: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(e, _)| e == name)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| {
            panic!(
                "Expected notification {name}, got {:?}",
                notifs.iter().map(|(e, _)| e).collect::<Vec<_>>()
            )
        })
}

// parity: notify:OnRaffleCreatedServer, notify:OnRaffleUpdatedServer, notify:OnRaffleFinishedServer
#[tokio::test]
async fn test_notify_live_hypernet_raffle_notifications() {
    eve_testkit::attest(&[
        "notify:OnRaffleCreatedServer",
        "notify:OnRaffleUpdatedServer",
        "notify:OnRaffleFinishedServer",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let prize_id = 640_000_110_i64;
    let mut sess_a = login_in_station(&ctx, 9_501, "demo", "demo-secret", PILOT_A).await;
    let mut sess_b = login_in_station(&ctx, 9_502, "other", "other-secret", PILOT_B).await;
    let mut sess_c = login_in_station(&ctx, 9_503, "test_c", "test_c-secret", PILOT_C).await;
    rpc(&ctx, &mut sess_c, "raffleProxy", "SubscribeToRaffle", &[]).await.expect("sub");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(prize_id, make_item(prize_id, 587, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true));
        snap.items.insert(prize_id + 1, make_item(prize_id + 1, 52_568, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 10, false));
    });
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    let raffle_rep = rpc(
        &ctx,
        &mut sess_a,
        "raffleProxy",
        "CreateRaffle",
        &[
            PyRep::Integer(prize_id),
            PyRep::Integer(8),
            PyRep::Integer(1_000_000),
        ],
    )
    .await
    .expect("CreateRaffle");
    let raffle_id = raffle_rep.dict_get("raffle_id").and_then(|v| v.as_i64()).expect("raffle_id");
    let create_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let created_args = find_notif(&create_notifs, "OnRaffleCreatedServer");
    assert_eq!(created_args[0], PyRep::Integer(raffle_id));
    assert_eq!(created_args[1].dict_get("raffle_id").and_then(|v| v.as_i64()), Some(raffle_id));
    
    let create_notifs_c = drain_all(&ctx, &mut sess_c, PILOT_C);
    let created_args_c = find_notif(&create_notifs_c, "OnRaffleCreatedServer");
    assert_eq!(created_args_c[0], PyRep::Integer(raffle_id));

    rpc(&ctx, &mut sess_b, "raffleProxy", "BuyRandomTickets", &[PyRep::Integer(raffle_id), PyRep::Integer(8)])
        .await
        .expect("BuyRandomTickets");
    let buy_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    let updated_args = find_notif(&buy_notifs_b, "OnRaffleUpdatedServer");
    assert_eq!(updated_args[0], PyRep::Integer(raffle_id));
    let finished_args = find_notif(&buy_notifs_b, "OnRaffleFinishedServer");
    assert_eq!(finished_args[0], PyRep::Integer(raffle_id));
    let win_char_id = finished_args[1].as_i64().expect("winning char_id");
    assert_eq!(win_char_id, PILOT_B as i64);

    let buy_notifs_c = drain_all(&ctx, &mut sess_c, PILOT_C);
    let updated_args_c = find_notif(&buy_notifs_c, "OnRaffleUpdatedServer");
    assert_eq!(updated_args_c[0], PyRep::Integer(raffle_id));
    let finished_args_c = find_notif(&buy_notifs_c, "OnRaffleFinishedServer");
    assert_eq!(finished_args_c[0], PyRep::Integer(raffle_id));
}
