//! Integration tests for Batch 34 Quasar Gateway Services:
//! - `eve.dailygoal.api` & `eve.dailygoal.api.admin`
//! - `eve.loyaltypoints.wallet` & `eve.loyaltypoints.assetholding.operation.api`
//! - `eve.solarsystem.bounty.npc` & `eve.solarsystem.bounty.bank.*`

#![forbid(unsafe_code)]

use crate::common;
use eve_db::IskCents;
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, DynamicValue, EvePublicNotice, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK, STATUS_PRECONDITION_FAILED,
};
use eve_server::router::ServerContext;
use eve_server::services::quasar_b34_services::{
    emit_ess_main_bank_link_established, emit_ess_main_bank_primed,
    emit_ess_main_bank_theft_completed, emit_ess_reserve_bank_primed,
};
use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                }
            }
        }
    }
    panic!("Expected KeyVal with response_proto, got {rep:?}");
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    value: Vec<u8>,
) -> EvePublicResponse {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: format!("b34-req-{n:06}").into_bytes(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"b34-instance-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("SendRequest");
    extract_response_proto(&rep)
}

fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<GoogleAny> {
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        if let Some(p) = n.payload {
            out.push(p);
        }
    }
    out
}

fn make_uuid_req(uuid: [u8; 16]) -> Vec<u8> {
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &uuid, &mut id_bytes);
    let mut out = Vec::new();
    encode_bytes_field(1, &id_bytes, &mut out);
    out
}

fn make_admin_progress_req(uuid: [u8; 16], cid: u32, delta: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    let mut c_bytes = Vec::new();
    encode_varint_field(1, u64::from(cid), &mut c_bytes);
    encode_bytes_field(2, &c_bytes, &mut out);
    encode_varint_field(3, delta, &mut out);
    out
}

fn make_admin_expire_req(uuid: [u8; 16], cid: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    let mut c_bytes = Vec::new();
    encode_varint_field(1, u64::from(cid), &mut c_bytes);
    encode_bytes_field(2, &c_bytes, &mut out);
    out
}

fn make_lp_op_req(op_uuid: [u8; 16], cid: Option<u32>, corp_id: Option<u32>, issuer: u32, amount: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &op_uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);

    let mut wallet_bytes = Vec::new();
    if let Some(c) = cid {
        let mut c_bytes = Vec::new();
        encode_varint_field(1, u64::from(c), &mut c_bytes);
        encode_bytes_field(1, &c_bytes, &mut wallet_bytes);
    } else if let Some(corp) = corp_id {
        let mut c_bytes = Vec::new();
        encode_varint_field(1, u64::from(corp), &mut c_bytes);
        encode_bytes_field(2, &c_bytes, &mut wallet_bytes);
    }
    let mut issuer_bytes = Vec::new();
    encode_varint_field(1, u64::from(issuer), &mut issuer_bytes);
    encode_bytes_field(3, &issuer_bytes, &mut wallet_bytes);

    let mut curr_bytes = Vec::new();
    encode_varint_field(1, amount, &mut curr_bytes);

    let mut attr_bytes = Vec::new();
    encode_bytes_field(1, &wallet_bytes, &mut attr_bytes);
    encode_bytes_field(2, &curr_bytes, &mut attr_bytes);
    encode_bytes_field(2, &attr_bytes, &mut out);
    out
}

// ============================================================================
// 1. Daily Goal Admin, API & Events Closure
// ============================================================================

#[tokio::test]
async fn test_b34_daily_goal_admin_and_events_closure() {
    // parity: quasar:eveProto/generated/eve/dailygoal/api/admin/requests_pb2.pyj, quasar:eveProto/generated/eve/dailygoal/api/api_pb2.pyj, quasar:eveProto/generated/eve/dailygoal/api/events_pb2.pyj
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let reg = QuasarDescriptorRegistry::open_default().expect("descriptor registry");

    let mut unpriv_sess = SessionState::new(1001);
    unpriv_sess.user_id = Some(1);
    unpriv_sess.character_id = Some(90_000_001);

    let mut admin_sess = SessionState::new(1002);
    admin_sess.user_id = Some(2);
    admin_sess.character_id = Some(90_000_001);
    admin_sess.role = eve_machonet::service_roles::ROLE_GM_MASK;
    let mut rx = ctx
        .quasar_gateway
        .subscribe_session(&admin_sess, Vec::new(), admin_sess.session_id);

    // Unprivileged admin call must be rejected with STATUS_FORBIDDEN
    let forb = quasar_rpc(
        &ctx,
        &mut unpriv_sess,
        "eve.dailygoal.api.admin.RegenerateCurrentGoalsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(forb.status_code, STATUS_FORBIDDEN);

    // Admin RegenerateCurrentGoalsRequest
    let regen_rsp = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve.dailygoal.api.admin.RegenerateCurrentGoalsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(regen_rsp.status_code, STATUS_OK);
    let regen_dyn = reg
        .decode_dynamic(
            "eve.dailygoal.api.admin.RegenerateCurrentGoalsResponse",
            &regen_rsp.payload.unwrap().value,
        )
        .expect("decode RegenerateCurrentGoalsResponse");
    assert!(regen_dyn.get(1).is_some(), "RegenerateCurrentGoalsResponse must contain goals");

    let notices = drain_notices(&mut rx);
    assert!(
        notices.iter().any(|n| n.normalized_type_name() == "eve.dailygoal.api.PeriodRegenerated"),
        "Expected PeriodRegenerated notice"
    );
    assert!(
        notices.iter().any(|n| n.normalized_type_name() == "eve.dailygoal.api.Created"),
        "Expected Created notice"
    );

    // Monthly regeneration
    let m_rsp = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve.dailygoal.api.admin.RegenerateCurrentMonthlyGoalsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(m_rsp.status_code, STATUS_OK);
    let _ = drain_notices(&mut rx);

    // Query goals via eve.dailygoal.api.GetAllRequest & GetRequest
    let all_rsp = quasar_rpc(&ctx, &mut admin_sess, "eve.dailygoal.api.GetAllRequest", Vec::new()).await;
    assert_eq!(all_rsp.status_code, STATUS_OK);
    let all_dyn = reg
        .decode_dynamic("eve.dailygoal.api.GetAllResponse", &all_rsp.payload.unwrap().value)
        .expect("decode GetAllResponse");
    let uuids: Vec<[u8; 16]> = match all_dyn.get(1) {
        Some(DynamicValue::Repeated(items)) => items
            .iter()
            .filter_map(|v| match v {
                DynamicValue::Message(id_msg) => match id_msg.get(1) {
                    Some(DynamicValue::Bytes(b)) => b.as_slice().try_into().ok(),
                    _ => None,
                },
                DynamicValue::Bytes(b) => {
                    let id_msg = reg.decode_dynamic("eve.dailygoal.Identifier", b).ok()?;
                    match id_msg.get(1) {
                        Some(DynamicValue::Bytes(b)) => b.as_slice().try_into().ok(),
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    assert!(uuids.len() >= 2, "Expected at least 2 daily goals");
    let g1 = uuids[0];
    let g2 = uuids[1];

    let get_rsp = quasar_rpc(&ctx, &mut admin_sess, "eve.dailygoal.api.GetRequest", make_uuid_req(g1)).await;
    assert_eq!(get_rsp.status_code, STATUS_OK);

    // Admin ProgressRequest -> Progressed + Completed -> AutoRedeemRequest -> Redeemed
    let prog_rsp = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve.dailygoal.api.admin.ProgressRequest",
        make_admin_progress_req(g1, 90_000_001, 10_000),
    )
    .await;
    assert_eq!(prog_rsp.status_code, STATUS_OK);
    let prog_notices = drain_notices(&mut rx);
    assert!(prog_notices.iter().any(|n| n.normalized_type_name() == "eve.dailygoal.api.Progressed"));
    assert!(prog_notices.iter().any(|n| n.normalized_type_name() == "eve.dailygoal.api.Completed"));

    let gp_rsp = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve.dailygoal.api.GetProgressRequest",
        make_uuid_req(g1),
    )
    .await;
    assert_eq!(gp_rsp.status_code, STATUS_OK);
    let gp_dyn = reg
        .decode_dynamic("eve.dailygoal.api.GetProgressResponse", &gp_rsp.payload.unwrap().value)
        .expect("decode GetProgressResponse");
    assert!(gp_dyn.get(4).is_some(), "Completed goal must have completed timestamp (field 4)");

    let auto_rsp = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve.dailygoal.api.admin.AutoRedeemRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(auto_rsp.status_code, STATUS_OK);
    let auto_notices = drain_notices(&mut rx);
    assert!(auto_notices.iter().any(|n| n.normalized_type_name() == "eve.dailygoal.api.Redeemed"));

    // Admin ExpireRequest -> PrematurelyDeactivated + Removed
    let exp_rsp = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve.dailygoal.api.admin.ExpireRequest",
        make_admin_expire_req(g2, 90_000_001),
    )
    .await;
    assert_eq!(exp_rsp.status_code, STATUS_OK);
    let exp_notices = drain_notices(&mut rx);
    assert!(exp_notices
        .iter()
        .any(|n| n.normalized_type_name() == "eve.dailygoal.api.PrematurelyDeactivated"));
    assert!(exp_notices.iter().any(|n| n.normalized_type_name() == "eve.dailygoal.api.Removed"));

    let gone_rsp = quasar_rpc(&ctx, &mut admin_sess, "eve.dailygoal.api.GetRequest", make_uuid_req(g2)).await;
    assert_eq!(gone_rsp.status_code, STATUS_NOT_FOUND);
}

// ============================================================================
// 2. Loyalty Points Wallet & AssetHolding Operation Closure
// ============================================================================

#[tokio::test]
async fn test_b34_loyalty_points_wallet_assetholding_closure() {
    // parity: quasar:eveProto/generated/eve/loyaltypoints/wallet/wallet_pb2.pyj, quasar:eveProto/generated/eve/loyaltypoints/wallet/api/api_pb2.pyj, quasar:eveProto/generated/eve/loyaltypoints/wallet/assetholding/operation/operation_pb2.pyj, quasar:eveProto/generated/eve/loyaltypoints/wallet/assetholding/operation/api/requests_pb2.pyj, quasar:eveProto/generated/eve/loyaltypoints/wallet/assetholding/operation/api/events_pb2.pyj
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let reg = QuasarDescriptorRegistry::open_default().expect("descriptor registry");

    let cid: u32 = 90_000_001;
    let issuer_corp: u32 = 1_000_035;
    let mut sess = SessionState::new(1001);
    sess.user_id = Some(1);
    sess.character_id = Some(i64::from(cid));
    let mut rx = ctx
        .quasar_gateway
        .subscribe_session(&sess, Vec::new(), sess.session_id);

    let dep_uuid = [0x11u8; 16];
    let dep_req = make_lp_op_req(dep_uuid, Some(cid), None, issuer_corp, 2_500);
    let dep_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
        dep_req.clone(),
    )
    .await;
    assert_eq!(dep_rsp.status_code, STATUS_OK);

    // Idempotent duplicate deposit must not double-credit
    let dep_dup = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
        dep_req,
    )
    .await;
    assert_eq!(dep_dup.status_code, STATUS_OK);

    let notices = drain_notices(&mut rx);
    let bal_added = notices
        .iter()
        .find(|n| n.normalized_type_name() == "eve.loyaltypoints.wallet.api.BalanceAdded")
        .unwrap_or_else(|| panic!("BalanceAdded notice not found in {notices:?}"));
    let bal_dyn = reg
        .decode_dynamic(
            "eve.loyaltypoints.wallet.api.BalanceAdded",
            &bal_added.value,
        )
        .expect("decode BalanceAdded");
    assert_eq!(bal_dyn.get(2), Some(&DynamicValue::Uint64(2_500)));
    assert!(notices
        .iter()
        .any(|n| n.normalized_type_name() == "eve.loyaltypoints.assetholding.operation.api.Deposited"));
    assert!(notices
        .iter()
        .any(|n| n.normalized_type_name() == "eve.loyaltypoints.assetholding.operation.api.Ended"));

    // Withdraw more than balance -> STATUS_PRECONDITION_FAILED
    let over_wd = make_lp_op_req([0x22u8; 16], Some(cid), None, issuer_corp, 9_999);
    let over_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.loyaltypoints.assetholding.operation.api.WithdrawRequest",
        over_wd,
    )
    .await;
    assert_eq!(over_rsp.status_code, STATUS_PRECONDITION_FAILED);

    // Valid withdraw of 1_000 LP
    let wd_req = make_lp_op_req([0x33u8; 16], Some(cid), None, issuer_corp, 1_000);
    let wd_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.loyaltypoints.assetholding.operation.api.WithdrawRequest",
        wd_req,
    )
    .await;
    assert_eq!(wd_rsp.status_code, STATUS_OK);

    let wd_notices = drain_notices(&mut rx);
    assert!(wd_notices
        .iter()
        .any(|n| n.normalized_type_name() == "eve.loyaltypoints.wallet.api.BalanceRemoved"));
    assert!(wd_notices
        .iter()
        .any(|n| n.normalized_type_name() == "eve.loyaltypoints.assetholding.operation.api.Withdrawn"));
    assert!(wd_notices
        .iter()
        .any(|n| n.normalized_type_name() == "eve.loyaltypoints.assetholding.operation.api.Ended"));
}

// ============================================================================
// 3. SolarSystem Bounty NPC Multiplier & ESS Bank Notices Closure
// ============================================================================

#[tokio::test]
async fn test_b34_solarsystem_bounty_npc_and_ess_bank_closure() {
    // parity: quasar:eveProto/generated/eve/solarsystem/bounty/npc_pb2.pyj, quasar:eveProto/generated/eve/solarsystem/bounty/bank/bank_pb2.pyj, quasar:eveProto/generated/eve/solarsystem/bounty/bank/mainbank_pb2.pyj, quasar:eveProto/generated/eve/solarsystem/bounty/bank/reservebank_pb2.pyj
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let reg = QuasarDescriptorRegistry::open_default().expect("descriptor registry");

    let sys_id: u32 = 30_000_142;
    let cid: u32 = 9001;
    let mut sess = SessionState::new(1001);
    sess.user_id = Some(1);
    sess.character_id = Some(i64::from(cid));
    sess.solar_system_id = Some(sys_id);
    let mut rx = ctx
        .quasar_gateway
        .subscribe_session(&sess, Vec::new(), sess.session_id);

    // Invalid solar_system_id 0 -> STATUS_BAD_REQUEST
    let bad_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.solarsystem.bounty.npc.ApplyMultiplierFactorRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(bad_rsp.status_code, STATUS_BAD_REQUEST);

    // Valid ApplyMultiplierFactorRequest (135% -> 1.35)
    let mut req = Vec::new();
    let mut s_bytes = Vec::new();
    encode_varint_field(1, u64::from(sys_id), &mut s_bytes);
    encode_bytes_field(1, &s_bytes, &mut req);
    encode_varint_field(2, 135, &mut req);
    let mut det = Vec::new();
    encode_varint_field(1, 1, &mut det);
    encode_varint_field(2, 350_000_000, &mut det);
    encode_bytes_field(3, &det, &mut req);

    let ok_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.solarsystem.bounty.npc.ApplyMultiplierFactorRequest",
        req,
    )
    .await;
    assert_eq!(ok_rsp.status_code, STATUS_OK);

    let notices = drain_notices(&mut rx);
    let calc_n = notices
        .iter()
        .find(|n| n.normalized_type_name() == "eve.solarsystem.bounty.npc.MultiplierFactorCalculated")
        .expect("MultiplierFactorCalculated notice");
    let calc_dyn = reg
        .decode_dynamic(
            "eve.solarsystem.bounty.npc.MultiplierFactorCalculated",
            &calc_n.value,
        )
        .expect("decode MultiplierFactorCalculated");
    assert_eq!(calc_dyn.get(2), Some(&DynamicValue::Uint32(135)));
    assert!(notices
        .iter()
        .any(|n| n.normalized_type_name() == "eve.solarsystem.bounty.bank.SolarSystemPersisted"));

    // Emit ESS MainBank & ReserveBank notices and decode them via descriptor registry
    emit_ess_main_bank_link_established(
        &ctx.quasar_gateway,
        cid,
        sys_id,
        700_001,
        IskCents::from_isk(5_000_000),
        IskCents::from_isk(20_000_000),
        ctx.now_unix(),
    );
    emit_ess_main_bank_primed(
        &ctx.quasar_gateway,
        sys_id,
        IskCents::from_isk(5_000_000),
        ctx.now_unix(),
    );
    emit_ess_main_bank_theft_completed(
        &ctx.quasar_gateway,
        cid,
        sys_id,
        IskCents::from_isk(20_000_000),
        IskCents::from_isk(5_000_000),
        IskCents(0),
        ctx.now_unix(),
    );
    emit_ess_reserve_bank_primed(
        &ctx.quasar_gateway,
        sys_id,
        IskCents::from_isk(20_000_000),
        ctx.now_unix(),
    );

    let ess_notices = drain_notices(&mut rx);
    for expected in [
        "eve.solarsystem.bounty.bank.main.MainBankLinkEstablished",
        "eve.solarsystem.bounty.bank.main.MainBankPrimed",
        "eve.solarsystem.bounty.bank.main.MainBankTheftCompleted",
        "eve.solarsystem.bounty.bank.reserve.ReserveBankPrimed",
    ] {
        let n = ess_notices
            .iter()
            .find(|n| n.normalized_type_name() == expected)
            .unwrap_or_else(|| panic!("Missing ESS notice {expected}"));
        reg.decode_dynamic(expected, &n.value)
            .unwrap_or_else(|e| panic!("Failed to decode {expected}: {e}"));
    }
}
