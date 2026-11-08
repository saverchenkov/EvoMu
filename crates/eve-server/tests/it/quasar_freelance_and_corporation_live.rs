//! V24 freelance projects driven end to end by a normal (non-GM) player session.
//!
//! The founder logs in, founds a corporation with `corpRegistry.AddCorporation`, funds its wallet with
//! `account.GiveCash`, then drives `eve_public.freelance.project.api` and
//! `eve_public.freelance.contributionmethod.itemdelivery.api` through `publicGatewaySvc.SendRequest`.
//! Item delivery moves starter Tritanium into the corporation deliveries hangar; a `ManufactureItems`
//! project progresses from a real `industryManager` Rifter job. Field numbers follow the V24
//! descriptors in `data/quasar_descriptors.sqlite`.

#![forbid(unsafe_code)]

use crate::quasar_campaign_lifecycle::{field, ok, quasar, sub, subs, subscribe, varint};
use eve_db::{InventoryFlag, IskCents};
use eve_machonet::service_roles::ROLE_GM_MASK;
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, encode_zigzag64, EvePublicNotice, WireMessage,
    WireValue,
};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;

const FOUNDER: i64 = 90_000_001;
const JITA_44: i64 = 60_003_760;
const TRITANIUM: u32 = 34;
const RIFTER: u32 = 587;
const RIFTER_BPO_ITEM: i64 = 100_000_007;
const RIFTER_BPO_TYPE: i64 = 691;
const STARTER_TRITANIUM: i64 = 100_000_008;
const OTHER_PILOT_GUN: i64 = 100_000_006;
const PROJECT_API: &str = "eve_public.freelance.project.api";

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method}: {e:?}"))
}

/// Logs the demo pilot in, founds a player corporation and funds its master wallet with `budget_isk`.
async fn founder_with_corp(ctx: &ServerContext, budget_isk: f64) -> (SessionState, i64) {
    let mut sess = SessionState::new(7_301);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(FOUNDER)]).await;
    assert_eq!(sess.role & ROLE_GM_MASK, 0, "freelance flows must run under a player session");
    ctx.advance_virtual_clock(12); // login session-change timer
    let created = rpc(
        ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Freelance Haulers".into()), PyRep::String("FLHL".into()), PyRep::Real(0.1)],
    )
    .await;
    let PyRep::KeyVal(kv) = &created else { panic!("AddCorporation returns KeyVal, got {created:?}") };
    let corp_id = kv["corporationID"].as_i64().expect("corporationID");
    assert_eq!(sess.corporation_id, Some(corp_id));
    ctx.advance_virtual_clock(12); // corp-change session timer
    rpc(ctx, &mut sess, "account", "GiveCash", &[PyRep::Integer(corp_id), PyRep::Real(budget_isk), PyRep::String("project budget".into())]).await;
    (sess, corp_id)
}

fn corp_wallet(ctx: &ServerContext, corp_id: i64) -> IskCents {
    ctx.db.with_snapshot(|s| {
        let c = &s.corporations[&corp_id];
        if c.master_wallet_balance().raw_cents() > 0 { c.master_wallet_balance() } else { c.division_balance(1) }
    })
}

fn currency(units: i64) -> Vec<u8> {
    let mut c = Vec::new();
    encode_varint_field(1, encode_zigzag64(units), &mut c);
    c
}

/// `CreateRequest` with a single-parameter configuration, `Reward.payout_per_unit` and
/// `CreationCost.reward_deposit` set explicitly.
fn create_request(name: &str, target: u64, method: &str, type_id: u32, payout: i64, deposit: i64, deadline: i64) -> Vec<u8> {
    let mut method_id = Vec::new();
    encode_string_field(1, method, &mut method_id);
    let mut entry = Vec::new();
    encode_string_field(1, "typeID", &mut entry);
    encode_string_field(2, &type_id.to_string(), &mut entry);
    let mut matcher = Vec::new();
    encode_bytes_field(1, &entry, &mut matcher);
    let mut param = Vec::new();
    if method == "DeliverItem" {
        encode_string_field(1, "corporation_item_delivery", &mut param);
        let mut cid = Vec::new();
        encode_bytes_field(1, &matcher, &mut cid);
        encode_bytes_field(5, &cid, &mut param);
    } else {
        encode_string_field(1, "item_type", &mut param);
        encode_bytes_field(2, &matcher, &mut param);
    }
    let mut cfg = Vec::new();
    encode_bytes_field(1, &method_id, &mut cfg);
    encode_bytes_field(3, &param, &mut cfg);
    let mut ts = Vec::new();
    encode_varint_field(1, deadline as u64, &mut ts);
    let mut reward = Vec::new();
    encode_bytes_field(1, &currency(payout), &mut reward);
    let mut cost = Vec::new();
    encode_bytes_field(3, &currency(deposit), &mut cost);
    let mut out = Vec::new();
    encode_string_field(1, name, &mut out);
    encode_string_field(2, "Live freelance project", &mut out);
    encode_varint_field(3, target, &mut out);
    encode_bytes_field(5, &ts, &mut out);
    encode_bytes_field(6, &cfg, &mut out);
    encode_bytes_field(10, &reward, &mut out);
    encode_bytes_field(11, &cost, &mut out);
    out
}

fn project_id(uuid: &[u8]) -> Vec<u8> {
    let mut inner = Vec::new();
    encode_bytes_field(1, uuid, &mut inner);
    let mut out = Vec::new();
    encode_bytes_field(1, &inner, &mut out);
    out
}

/// `DeliverRequest` naming the station delivery location, the item and its hangar location explicitly.
fn deliver_request(uuid: &[u8], station: i64, item: i64, qty: u64) -> Vec<u8> {
    let seq = |id: i64| {
        let mut b = Vec::new();
        encode_varint_field(1, id as u64, &mut b);
        b
    };
    let mut loc = Vec::new();
    encode_bytes_field(1, &seq(station), &mut loc);
    let mut item_loc = Vec::new();
    encode_bytes_field(1, &seq(station), &mut item_loc);
    encode_varint_field(2, InventoryFlag::Hangar as u64, &mut item_loc);
    let mut out = project_id(uuid);
    encode_bytes_field(2, &loc, &mut out);
    encode_bytes_field(3, &seq(item), &mut out);
    encode_varint_field(4, qty, &mut out);
    encode_bytes_field(5, &item_loc, &mut out);
    out
}

fn bytes_of(wm: &WireMessage, num: u32) -> Vec<u8> {
    match field(wm, num) {
        Some(WireValue::LengthDelimited(b)) => b.clone(),
        other => panic!("field {num} is not length-delimited: {other:?}"),
    }
}

fn has_field(wm: &WireMessage, num: u32) -> bool {
    field(wm, num).is_some()
}

async fn create(ctx: &ServerContext, sess: &mut SessionState, payload: Vec<u8>) -> Vec<u8> {
    let rsp = ok(ctx, sess, &format!("{PROJECT_API}.CreateRequest"), payload).await;
    let uuid = bytes_of(&sub(&sub(&rsp, 1), 1), 1);
    assert_eq!(uuid.len(), 16, "CreateResponse.created.project.uuid");
    uuid
}

/// Drains the stream keeping `eve_public.freelance.*` notices as (short type name, payload).
fn freelance_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<(String, WireMessage)> {
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        let p = n.payload.expect("notice payload");
        let ty = p.normalized_type_name().to_string();
        if let Some(short) = ty.strip_prefix("eve_public.freelance.") {
            out.push((short.to_string(), WireMessage::decode(&p.value).expect("decode notice")));
        }
    }
    out
}

fn notice<'a>(all: &'a [(String, WireMessage)], ty: &str) -> &'a WireMessage {
    &all.iter().find(|(t, _)| t == ty).unwrap_or_else(|| panic!("missing notice {ty}; got {:?}", all.iter().map(|(t, _)| t).collect::<Vec<_>>())).1
}

fn item_qty(ctx: &ServerContext, owner: i64, type_id: u32, flag: InventoryFlag) -> i64 {
    ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|i| i.owner_id == owner && i.type_id == type_id && i.location_id == JITA_44 && i.flag == flag)
            .map(|i| i64::from(i.quantity))
            .sum()
    })
}

#[tokio::test]
async fn freelance_item_delivery_reward_close_and_pagination_under_player_session() {
    // parity: feat:quasar.freelance, quasar:eveProto/generated/eve_public/freelance/project/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/project/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/notices_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.freelance",
        "quasar:eveProto/generated/eve_public/freelance/project/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/project/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/notices_pb2.pyj",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (mut sess, corp_id) = founder_with_corp(&ctx, 20_000_000.0).await;
    let mut rx = subscribe(&ctx, &mut sess).await;
    let wallet_funded = corp_wallet(&ctx, corp_id);
    let deadline = ctx.now_unix() as i64 + 7 * 86_400;

    // Explicit reward budget: 1,000 ISK per Tritanium, 1,000,000 ISK deposit, nothing else is assumed.
    let uuid = create(&ctx, &mut sess, create_request("Tritanium drive", 1_000, "DeliverItem", TRITANIUM, 1_000, 1_000_000, deadline)).await;
    assert_eq!(wallet_funded.raw_cents() - corp_wallet(&ctx, corp_id).raw_cents(), IskCents::from_isk(1_000_000).raw_cents());

    let mgr = ok(&ctx, &mut sess, &format!("{PROJECT_API}.GetForManagerRequest"), project_id(&uuid)).await;
    let attrs = sub(&mgr, 1);
    assert_eq!(varint(&sub(&attrs, 1), 1), Some(corp_id as u64), "Attributes.owner");
    assert_eq!(varint(&sub(&attrs, 2), 1), Some(FOUNDER as u64), "Attributes.creator");
    assert_eq!(varint(&attrs, 3), Some(1), "STATE_ACTIVE");
    assert_eq!(varint(&attrs, 11), Some(1_000), "target_progress");

    let commit = ok(&ctx, &mut sess, &format!("{PROJECT_API}.CommitRequest"), project_id(&uuid)).await;
    assert!(has_field(&commit, 1), "CommitResponse.committed");

    // Deliver 400 Tritanium from the Jita 4-4 hangar: they land in the corporation deliveries hangar.
    let hangar_before = item_qty(&ctx, FOUNDER, TRITANIUM, InventoryFlag::Hangar);
    let deliver_api = "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest";
    // Delivering from a station the pilot is not docked at, or someone else's item, is refused.
    let other_station = ctx.sde.stations.keys().copied().filter(|&st| st != JITA_44).min().expect("another SDE station");
    let refused = ok(&ctx, &mut sess, deliver_api, deliver_request(&uuid, other_station, STARTER_TRITANIUM, 400)).await;
    assert!(has_field(&refused, 5), "DeliverResponse.character_not_docked, got fields {:?}", refused.fields.iter().map(|f| f.field_number).collect::<Vec<_>>());
    let refused = ok(&ctx, &mut sess, deliver_api, deliver_request(&uuid, JITA_44, OTHER_PILOT_GUN, 1)).await;
    assert!(has_field(&refused, 6), "DeliverResponse.item_not_owned_by_deliverer");
    let delivered = ok(&ctx, &mut sess, deliver_api, deliver_request(&uuid, JITA_44, STARTER_TRITANIUM, 400)).await;
    assert_eq!(bytes_of(&sub(&delivered, 1), 1).len(), 18, "DeliverResponse.escrowed.delivery identifier");
    assert_eq!(item_qty(&ctx, FOUNDER, TRITANIUM, InventoryFlag::Hangar), hangar_before - 400);
    assert_eq!(item_qty(&ctx, corp_id, TRITANIUM, InventoryFlag::CorpDeliveries), 400);

    let notices = freelance_notices(&mut rx);
    let created = notice(&notices, "project.api.CreatedNotice");
    assert_eq!(bytes_of(&sub(created, 1), 1), uuid);
    assert_eq!(varint(&sub(created, 2), 11), Some(1_000));
    assert_eq!(varint(notice(&notices, "project.api.ParticipantAddedNotice"), 2), Some(1));
    let succeeded = notice(&notices, "contributionmethod.itemdelivery.api.ContributionSucceededNotice");
    assert_eq!((varint(succeeded, 2), varint(succeeded, 3)), (Some(400), Some(600)));
    let progressed = notice(&notices, "project.api.ProgressedNotice");
    assert_eq!((varint(progressed, 2), varint(progressed, 3)), (Some(400), Some(1)));
    let contributed = notice(&notices, "project.api.ParticipantContributedNotice");
    assert_eq!(varint(&sub(contributed, 2), 1), Some(FOUNDER as u64));
    assert_eq!(varint(contributed, 3), Some(400));

    // Redeem pays exactly 400 x 1,000 ISK from escrow.
    let bal = |ctx: &ServerContext| ctx.db.with_snapshot(|s| s.characters[&FOUNDER].balance);
    let before = bal(&ctx);
    ok(&ctx, &mut sess, &format!("{PROJECT_API}.RedeemRewardRequest"), project_id(&uuid)).await;
    assert_eq!(bal(&ctx).raw_cents() - before.raw_cents(), IskCents::from_isk(400_000).raw_cents());
    let redeemed = freelance_notices(&mut rx);
    assert_eq!(varint(&sub(notice(&redeemed, "project.api.ParticipantRewardRedeemedNotice"), 2), 1), Some(FOUNDER as u64));

    // Close refunds the 600,000 ISK escrow remainder and emits ClosedNotice; commits are then refused.
    let wallet_before_close = corp_wallet(&ctx, corp_id);
    ok(&ctx, &mut sess, &format!("{PROJECT_API}.CloseRequest"), project_id(&uuid)).await;
    assert_eq!(corp_wallet(&ctx, corp_id).raw_cents() - wallet_before_close.raw_cents(), IskCents::from_isk(600_000).raw_cents());
    let closed = freelance_notices(&mut rx);
    assert_eq!(bytes_of(&sub(notice(&closed, "project.api.ClosedNotice"), 1), 1), uuid);
    let recommit = ok(&ctx, &mut sess, &format!("{PROJECT_API}.CommitRequest"), project_id(&uuid)).await;
    assert!(has_field(&recommit, 3), "CommitResponse.invalid_project_state after close");

    // A second closed project gives two inactive projects to page through with Page{size=1}.
    let uuid2 = create(&ctx, &mut sess, create_request("Second drive", 10, "DeliverItem", TRITANIUM, 100, 1_000, deadline)).await;
    ok(&ctx, &mut sess, &format!("{PROJECT_API}.CloseRequest"), project_id(&uuid2)).await;
    let page_req = |token: &str| {
        let mut page = Vec::new();
        encode_varint_field(1, 1, &mut page);
        if !token.is_empty() {
            encode_string_field(2, token, &mut page);
        }
        let mut out = Vec::new();
        encode_bytes_field(3, &page, &mut out);
        out
    };
    let first = ok(&ctx, &mut sess, &format!("{PROJECT_API}.GetAllInactiveRequest"), page_req("")).await;
    assert_eq!(subs(&first, 1).len(), 1);
    let token = String::from_utf8(bytes_of(&sub(&first, 2), 1)).expect("NextPage.token");
    let second = ok(&ctx, &mut sess, &format!("{PROJECT_API}.GetAllInactiveRequest"), page_req(&token)).await;
    assert_eq!(subs(&second, 1).len(), 1);
    assert!(!has_field(&second, 2), "last page carries no NextPage");
    let mut paged: Vec<Vec<u8>> = [&first, &second].iter().map(|p| bytes_of(&subs(p, 1)[0], 1)).collect();
    paged.sort();
    let mut expected = vec![uuid.clone(), uuid2.clone()];
    expected.sort();
    assert_eq!(paged, expected);
}

#[tokio::test]
async fn freelance_manufacture_items_project_progresses_from_real_industry_job() {
    // parity: feat:quasar.freelance
    eve_testkit::attest(&["feat:quasar.freelance"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (mut sess, corp_id) = founder_with_corp(&ctx, 5_000_000.0).await;
    let mut rx = subscribe(&ctx, &mut sess).await;
    let deadline = ctx.now_unix() as i64 + 7 * 86_400;

    let uuid = create(&ctx, &mut sess, create_request("Rifter line", 2, "ManufactureItems", RIFTER, 50_000, 100_000, deadline)).await;
    let got = ok(&ctx, &mut sess, &format!("{PROJECT_API}.GetForManagerRequest"), project_id(&uuid)).await;
    assert_eq!(varint(&sub(&sub(&got, 1), 1), 1), Some(corp_id as u64));
    ok(&ctx, &mut sess, &format!("{PROJECT_API}.CommitRequest"), project_id(&uuid)).await;

    // Item delivery cannot progress a manufacturing project (the starter Rifter hull stays in the hangar).
    let mut deliver = project_id(&uuid);
    encode_varint_field(4, 1, &mut deliver);
    let refused = ok(&ctx, &mut sess, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", deliver).await;
    assert!(has_field(&refused, 12), "DeliverResponse.project_does_not_allow_item_delivery");
    assert_eq!(item_qty(&ctx, FOUNDER, RIFTER, InventoryFlag::Hangar), 1);

    let job = rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(RIFTER_BPO_ITEM), PyRep::Integer(RIFTER_BPO_TYPE), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(JITA_44)],
    )
    .await;
    let job_id = job.dict_get("jobID").and_then(|v| v.as_i64()).expect("jobID");
    let dur = job.dict_get("durationSeconds").and_then(|v| v.as_i64()).expect("durationSeconds") as u64;
    ctx.advance_virtual_clock(dur + 5);
    rpc(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(job_id)]).await;

    let after = ok(&ctx, &mut sess, &format!("{PROJECT_API}.GetRequest"), project_id(&uuid)).await;
    assert_eq!(varint(&sub(&after, 1), 12), Some(1), "Attributes.current_progress after one Rifter");
    assert_eq!(varint(&after, 3), Some(1), "GetResponse.progress for the caller");
    let notices = freelance_notices(&mut rx);
    let progressed = notice(&notices, "project.api.ProgressedNotice");
    assert_eq!(varint(progressed, 2), Some(1));
    let contributed = notice(&notices, "project.api.ParticipantContributedNotice");
    assert_eq!(varint(&sub(contributed, 2), 1), Some(FOUNDER as u64));
}

fn corp_req(corp_id: i64, field_num: u32) -> Vec<u8> {
    let mut id = Vec::new();
    encode_varint_field(1, corp_id as u64, &mut id);
    let mut out = Vec::new();
    encode_bytes_field(field_num, &id, &mut out);
    out
}

fn sint(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

#[tokio::test]
async fn quasar_corporation_endpoints_reflect_live_corp_registry_industry_and_market_activity() {
    // parity: feat:quasar.corporation
    eve_testkit::attest(&["feat:quasar.corporation"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (mut sess, corp_id) = founder_with_corp(&ctx, 3_000_000.0).await;

    // marketProxy: the corporation buys Tritanium from the cheapest Jita 4-4 NPC sell order.
    let (npc_price, npc_qty) = ctx.db.with_snapshot(|s| {
        s.market_orders
            .values()
            .filter(|o| !o.is_bid && o.is_npc && o.station_id == JITA_44 && o.type_id == TRITANIUM && o.vol_remaining >= 100)
            .map(|o| (o.price, o.vol_remaining))
            .min_by_key(|(p, _)| p.raw_cents())
            .expect("seeded Jita Tritanium NPC sell order")
    });
    assert!(npc_qty >= 100);
    let buy_args = [
        PyRep::Integer(JITA_44),
        PyRep::Integer(i64::from(TRITANIUM)),
        PyRep::Real(npc_price.to_f64_isk()),
        PyRep::Integer(100),
        PyRep::Integer(-1),
        PyRep::Integer(1),
        PyRep::Integer(0),
        PyRep::Bool(true),
    ];
    rpc(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &buy_args).await;
    let spent = npc_price.checked_mul_qty(100).expect("trade value");

    // industryManager: the CEO installs a Rifter manufacturing job at Jita 4-4.
    let job = rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(RIFTER_BPO_ITEM), PyRep::Integer(RIFTER_BPO_TYPE), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(JITA_44)],
    )
    .await;
    let job_id = job.dict_get("jobID").and_then(|v| v.as_i64()).expect("jobID");

    // The corporation purchase lands in the corporation Deliveries hangar at Jita 4-4.
    assert_eq!(item_qty(&ctx, corp_id, TRITANIUM, InventoryFlag::Deliveries), 100);

    let balances = ok(&ctx, &mut sess, "eve.corporation.accounting.wallet.GetAllDivisionBalanceRequest", corp_req(corp_id, 2)).await;
    let master = subs(&balances, 1).into_iter().find(|d| varint(d, 1) == Some(1000)).expect("division 1000");
    let expected_units = (IskCents::from_isk(3_000_000).raw_cents() - spent.raw_cents()) / 100;
    assert_eq!(sint(varint(&sub(&master, 2), 1).expect("units")), expected_units);
    assert_eq!(corp_wallet(&ctx, corp_id).raw_cents() / 100, expected_units);

    let members = ok(&ctx, &mut sess, "eve.corporation.members.GetRequest", corp_req(corp_id, 1)).await;
    let ids: Vec<u64> = subs(&members, 1).iter().filter_map(|m| varint(m, 1)).collect();
    assert_eq!(ids, vec![FOUNDER as u64]);
    let roles = ok(&ctx, &mut sess, "eve.corporation.members.roles.GetRequest", corp_req(corp_id, 1)).await;
    let ceo_roles = subs(&roles, 1).into_iter().find(|r| varint(&sub(r, 1), 1) == Some(FOUNDER as u64)).expect("CEO roles");
    assert_ne!(varint(&ceo_roles, 2).unwrap_or(0) & eve_db::corp_roles::ROLE_DIRECTOR, 0);

    let jobs = ok(&ctx, &mut sess, "eve.corporation.industry.job.GetActiveRequest", corp_req(corp_id, 1)).await;
    let entry = subs(&jobs, 1).into_iter().find(|e| varint(&sub(e, 1), 1) == Some(job_id as u64)).expect("corp job entry");
    let manufacturing = sub(&entry, 4);
    assert_eq!(varint(&sub(&manufacturing, 3), 1), Some(FOUNDER as u64), "installer");
    assert_eq!(varint(&sub(&manufacturing, 4), 1), Some(JITA_44 as u64), "facility");
    assert_eq!(varint(&manufacturing, 8), Some(1), "runs");
    assert_eq!(varint(&sub(&manufacturing, 23), 1), Some(u64::from(RIFTER)), "product");

    let entries = ok(&ctx, &mut sess, "eve.corporation.market.entry.GetEntriesRequest", Vec::new()).await;
    let bought = subs(&entries, 1).into_iter().find(|e| varint(&sub(e, 2), 14).is_some() || has_field(e, 2)).expect("market entry");
    let attrs = sub(&bought, 2);
    assert_eq!(varint(&attrs, 4), Some(100), "quantity");
    assert_eq!(varint(&sub(&attrs, 14), 1), Some(u64::from(TRITANIUM)), "type");
    assert_eq!(varint(&bought, 3), Some(1), "buy side");

    // A pilot outside the corporation is refused.
    let mut outsider = SessionState::new(7_302);
    rpc(&ctx, &mut outsider, "authentication", "Login", &[PyRep::String("other".into()), PyRep::String("other-secret".into())]).await;
    rpc(&ctx, &mut outsider, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)]).await;
    let denied = quasar(&ctx, &mut outsider, "eve.corporation.members.GetRequest", corp_req(corp_id, 1)).await;
    assert_eq!(denied.status_code, eve_server::quasar_gateway::STATUS_FORBIDDEN);
}
