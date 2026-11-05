//! Live integration suite for Quasar Entitlements (`feat:quasar.entitlement` + all 17
//! `eveProto/generated/{eve,eve_public}/entitlement/**` descriptors) and The Fulcrum
//! Alignment Services in Zarzakh (`feat:structures.fulcrum-alignment-services`).

#![forbid(unsafe_code)]

use crate::common;
use eve_db::types::InventoryFlag;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicNotice,
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry,
    QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;

const JITA_SYS: u32 = 30_000_142;
const PARAGON_JITA_STATION: i64 = 60_015_169;
const PARAGON_CORP_ID: i64 = 1_000_419;
const ZARZAKH_SYS: u32 = 30_100_000;
const FULCRUM_STATION: i64 = 60_015_187;
const ANGEL_LP_CORP: i64 = 1_000_138;
const GURISTAS_LP_CORP: i64 = 1_000_127;

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

fn seq_msg(val: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, val, &mut b);
    b
}

fn user_seq_msg(uid: u64) -> Vec<u8> {
    seq_msg(eve_proto::quasar::encode_zigzag32(uid as i32) as u64)
}

fn logo_ident_msg(char_id: u64, ship_type_id: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_bytes_field(1, &seq_msg(char_id), &mut b);
    encode_bytes_field(2, &seq_msg(ship_type_id), &mut b);
    b
}

async fn q_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_url: &str,
    value: Vec<u8>,
) -> (u32, String, Vec<u8>) {
    let now = ctx.now_unix() as i64;
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: now, nanos: 0 }),
        correlation_uuid: b"corr-entitlement-live".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-entitlement-live".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = rpc(ctx, sess, "publicGatewaySvc", "SendRequest", &[PyRep::Bytes(req.encode_proto())])
        .await
        .expect("publicGatewaySvc.SendRequest");
    let PyRep::KeyVal(entries) = rep else { panic!("expected KeyVal response"); };
    for (k, v) in entries {
        if k == "response_proto" {
            if let PyRep::Bytes(b) = v {
                let rsp = EvePublicResponse::decode_proto(&b).expect("decode EvePublicResponse");
                let (turl, bytes) = rsp
                    .payload
                    .map(|p| (p.normalized_type_name().to_string(), p.value))
                    .unwrap_or_default();
                return (rsp.status_code, turl, bytes);
            }
        }
    }
    panic!("missing response_proto");
}

fn subscribe(ctx: &ServerContext, sess: &SessionState) -> Receiver<EvePublicNotice> {
    ctx.quasar_gateway.subscribe_session(sess, Vec::new(), ctx.now_unix())
}

fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<EvePublicNotice> {
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        out.push(n);
    }
    out
}

fn has_notice(notices: &[EvePublicNotice], expected: &str) -> bool {
    notices
        .iter()
        .any(|n| n.payload.as_ref().is_some_and(|p| p.normalized_type_name() == expected))
}

#[tokio::test]
async fn test_quasar_user_entitlement_and_subscription_live() {
    // parity: feat:quasar.entitlement, quasar:eveProto/generated/eve/entitlement/user/api_pb2.pyj, quasar:eveProto/generated/eve/entitlement/user/entitlement_pb2.pyj, quasar:eveProto/generated/eve/entitlement/user/subscription/api_pb2.pyj, quasar:eveProto/generated/eve/entitlement/user/subscription/subscription_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/user/api_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/user/entitlement_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.entitlement",
        "quasar:eveProto/generated/eve/entitlement/user/api_pb2.pyj", "quasar:eveProto/generated/eve/entitlement/user/entitlement_pb2.pyj",
        "quasar:eveProto/generated/eve/entitlement/user/subscription/api_pb2.pyj", "quasar:eveProto/generated/eve/entitlement/user/subscription/subscription_pb2.pyj",
        "quasar:eveProto/generated/eve_public/entitlement/user/api_pb2.pyj", "quasar:eveProto/generated/eve_public/entitlement/user/entitlement_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    ctx.set_clock_paused(true);

    let mut gm = common::gm_pilot_in_space(&ctx, 97_101, 97_100_001, 587, JITA_SYS).await;
    let pilot_uid = ctx.db.provision_player_account("alpha97102", "alpha97102-secret");
    let pilot_cid = ctx.db.create_character(pilot_uid, "Alpha Pilot 97102", 1, 1, 11, 17, &ctx.sde).expect("create alpha pilot").character_id;
    let mut pilot_sess = SessionState::new(97_102);
    common::login_pilot(&ctx, &mut pilot_sess, "alpha97102", "alpha97102-secret").await;
    rpc(&ctx, &mut pilot_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(pilot_cid)]).await.unwrap();
    let mut rx = subscribe(&ctx, &pilot_sess);

    // 1. Negative: unauthenticated, non-admin, and invalid requests return 401/403/400.
    let mut unauth = SessionState::new(97_199);
    let (code_unauth, _, _) = q_rpc(&ctx, &mut unauth, "eve_public.entitlement.user.api.GetRequest", Vec::new()).await;
    assert_eq!(code_unauth, 401);
    let (code_nongm_ent, _, _) = q_rpc(&ctx, &mut pilot_sess, "eve.entitlement.user.api.GrantRequest", Vec::new()).await;
    assert_eq!(code_nongm_ent, 403);
    let (code_nongm_sub, _, _) = q_rpc(&ctx, &mut pilot_sess, "eve.entitlement.user.subscription.api.GrantRequest", Vec::new()).await;
    assert_eq!(code_nongm_sub, 403);
    let (code_bad_ent, _, _) = q_rpc(&ctx, &mut gm.sess, "eve.entitlement.user.api.GrantRequest", Vec::new()).await;
    assert_eq!(code_bad_ent, 400);
    let (code_bad_sub, _, _) = q_rpc(&ctx, &mut gm.sess, "eve.entitlement.user.subscription.api.GrantRequest", Vec::new()).await;
    assert_eq!(code_bad_sub, 400);

    // 2. Initial state: empty entitlements, Alpha clone grade (0).
    let (c0, t0, b0) = q_rpc(&ctx, &mut pilot_sess, "eve_public.entitlement.user.api.GetRequest", Vec::new()).await;
    assert_eq!(c0, 200);
    assert_eq!(t0, "eve_public.entitlement.user.api.GetResponse");
    assert_eq!(b0.len(), 0);
    assert_eq!(rpc(&ctx, &mut pilot_sess, "loginRewardFacilities", "get_my_clone_grade", &[]).await.unwrap(), PyRep::Integer(0));

    // 3. Grant timed entitlement -> GrantedNotice + GetRequest decodes name & expiry_date timestamp.
    let expected_at_exp = ctx.now_unix() + 3_600;
    let mut ent_msg = Vec::new();
    encode_bytes_field(2, &user_seq_msg(pilot_uid), &mut ent_msg);
    encode_string_field(3, "alliance_tournament_xx", &mut ent_msg);
    encode_bytes_field(4, &seq_msg(3_600), &mut ent_msg);
    let mut grant_req = Vec::new();
    encode_bytes_field(1, &ent_msg, &mut grant_req);

    let (cg, tg, _) = q_rpc(&ctx, &mut gm.sess, "eve.entitlement.user.api.GrantRequest", grant_req).await;
    assert_eq!(cg, 200);
    assert_eq!(tg, "eve.entitlement.user.api.GrantResponse");
    assert!(has_notice(&drain_notices(&mut rx), "eve_public.assetholding.entitlement.api.GrantedNotice"));

    // 4. Grant Omega subscription -> ActivatedNotice + GrantedNotice + clone grade 1 (Omega).
    let expected_omega_exp = ctx.now_unix() + 30 * 86_400;
    let mut sub_msg = Vec::new();
    encode_bytes_field(1, &user_seq_msg(pilot_uid), &mut sub_msg);
    encode_varint_field(3, 30, &mut sub_msg);
    encode_string_field(6, "omega", &mut sub_msg);
    let mut sub_req = Vec::new();
    encode_bytes_field(1, &sub_msg, &mut sub_req);

    let (cs, ts, _) = q_rpc(&ctx, &mut gm.sess, "eve.entitlement.user.subscription.api.GrantRequest", sub_req).await;
    assert_eq!(cs, 200);
    assert_eq!(ts, "eve.entitlement.user.subscription.api.GrantResponse");
    let sub_notices = drain_notices(&mut rx);
    assert!(has_notice(&sub_notices, "eve_public.user.license.api.ActivatedNotice"));
    assert!(has_notice(&sub_notices, "eve_public.assetholding.entitlement.api.GrantedNotice"));
    assert_eq!(rpc(&ctx, &mut pilot_sess, "loginRewardFacilities", "get_my_clone_grade", &[]).await.unwrap(), PyRep::Integer(1));

    let (c1, _, b1) = q_rpc(&ctx, &mut pilot_sess, "eve_public.entitlement.user.api.GetRequest", Vec::new()).await;
    assert_eq!(c1, 200);
    let wm = WireMessage::decode(&b1).unwrap();
    assert_eq!(wm.fields.len(), 2);
    let e0 = WireMessage::decode(wm.fields[0].value.as_bytes().unwrap()).unwrap();
    let e1 = WireMessage::decode(wm.fields[1].value.as_bytes().unwrap()).unwrap();
    assert_eq!(std::str::from_utf8(e0.fields[0].value.as_bytes().unwrap()).unwrap(), "alliance_tournament_xx");
    assert_eq!(WireMessage::decode(e0.fields[1].value.as_bytes().unwrap()).unwrap().fields[0].value.as_varint(), Some(expected_at_exp));
    assert_eq!(std::str::from_utf8(e1.fields[0].value.as_bytes().unwrap()).unwrap(), "omega");
    assert_eq!(WireMessage::decode(e1.fields[1].value.as_bytes().unwrap()).unwrap().fields[0].value.as_varint(), Some(expected_omega_exp));

    // 5. Advance past 1h entitlement expiry -> only 30-day omega remains active.
    ctx.advance_secs(3_700);
    let (c2, _, b2) = q_rpc(&ctx, &mut pilot_sess, "eve_public.entitlement.user.api.GetRequest", Vec::new()).await;
    assert_eq!(c2, 200);
    let wm2 = WireMessage::decode(&b2).unwrap();
    assert_eq!(wm2.fields.len(), 1);
    let rem = WireMessage::decode(wm2.fields[0].value.as_bytes().unwrap()).unwrap();
    assert_eq!(std::str::from_utf8(rem.fields[0].value.as_bytes().unwrap()).unwrap(), "omega");
}

#[tokio::test]
async fn test_quasar_character_ship_logo_entitlements_live() {
    // parity: quasar:eveProto/generated/eve/entitlement/character/admin_pb2.pyj, quasar:eveProto/generated/eve/entitlement/character/api_pb2.pyj, quasar:eveProto/generated/eve/entitlement/character/ship/admin/alliancelogo_pb2.pyj, quasar:eveProto/generated/eve/entitlement/character/ship/admin/corplogo_pb2.pyj, quasar:eveProto/generated/eve/entitlement/character/ship/alliancelogo_pb2.pyj, quasar:eveProto/generated/eve/entitlement/character/ship/corplogo_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/character_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/alliancelogo_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/corplogo_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/ship/alliancelogo_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/ship/corplogo_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve/entitlement/character/admin_pb2.pyj", "quasar:eveProto/generated/eve/entitlement/character/api_pb2.pyj",
        "quasar:eveProto/generated/eve/entitlement/character/ship/admin/alliancelogo_pb2.pyj", "quasar:eveProto/generated/eve/entitlement/character/ship/admin/corplogo_pb2.pyj",
        "quasar:eveProto/generated/eve/entitlement/character/ship/alliancelogo_pb2.pyj", "quasar:eveProto/generated/eve/entitlement/character/ship/corplogo_pb2.pyj",
        "quasar:eveProto/generated/eve_public/entitlement/character/character_pb2.pyj",
        "quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/alliancelogo_pb2.pyj", "quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/corplogo_pb2.pyj",
        "quasar:eveProto/generated/eve_public/entitlement/character/ship/alliancelogo_pb2.pyj", "quasar:eveProto/generated/eve_public/entitlement/character/ship/corplogo_pb2.pyj",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("QuasarDescriptorRegistry");
    for pyj in [
        "eveProto/generated/eve/entitlement/character/admin_pb2.pyj",
        "eveProto/generated/eve/entitlement/character/api_pb2.pyj",
        "eveProto/generated/eve/entitlement/character/ship/admin/alliancelogo_pb2.pyj",
        "eveProto/generated/eve/entitlement/character/ship/admin/corplogo_pb2.pyj",
        "eveProto/generated/eve/entitlement/character/ship/alliancelogo_pb2.pyj",
        "eveProto/generated/eve/entitlement/character/ship/corplogo_pb2.pyj",
        "eveProto/generated/eve_public/entitlement/character/character_pb2.pyj",
        "eveProto/generated/eve_public/entitlement/character/ship/admin/alliancelogo_pb2.pyj",
        "eveProto/generated/eve_public/entitlement/character/ship/admin/corplogo_pb2.pyj",
        "eveProto/generated/eve_public/entitlement/character/ship/alliancelogo_pb2.pyj",
        "eveProto/generated/eve_public/entitlement/character/ship/corplogo_pb2.pyj",
    ] {
        assert!(reg.get_file_by_pyj(pyj).is_some(), "missing descriptor {pyj}");
    }

    let ctx = ServerContext::new();
    ctx.set_clock_paused(true);

    let mut gm = common::gm_pilot_in_space(&ctx, 97_111, 97_100_011, 587, JITA_SYS).await;
    let mut pilot = common::gm_pilot_in_space(&ctx, 97_112, 97_100_012, 587, JITA_SYS).await;
    let cid = pilot.char_id as u64;
    let mut rx = subscribe(&ctx, &pilot.sess);

    // 1. Dock at Paragon Fulfillment Center & earn 1,000 EverMarks via live Daily Goals RPCs.
    let p_pos = ctx.sde.stations[&PARAGON_JITA_STATION].position;
    ctx.systems.get_or_spawn(JITA_SYS).set_ball_position(pilot.ship_id, p_pos).await;
    ctx.advance_secs(12);
    rpc(&ctx, &mut pilot.sess, "stationSvc", "Dock", &[PyRep::Integer(PARAGON_JITA_STATION), PyRep::Integer(pilot.ship_id)])
        .await
        .expect("Dock Paragon station");
    ctx.advance_secs(12);

    for day in 0..3 {
        if day > 0 {
            ctx.advance_secs(86_401);
        }
        let (_, _, dg_bytes) = q_rpc(&ctx, &mut pilot.sess, "eve_public.dailygoal.api.GetAllCurrentRequest", Vec::new()).await;
        let dg_wm = WireMessage::decode(&dg_bytes).unwrap();
        for idx in 0..dg_wm.fields.len().min(4) {
            let id_bytes = dg_wm.fields[idx].value.as_bytes().unwrap().to_vec();
            let mut dg_prog = Vec::new();
            encode_bytes_field(1, &id_bytes, &mut dg_prog);
            encode_bytes_field(2, &seq_msg(cid), &mut dg_prog);
            encode_varint_field(3, 5_000, &mut dg_prog);
            assert_eq!(q_rpc(&ctx, &mut gm.sess, "eve_public.dailygoal.api.admin.ProgressRequest", dg_prog).await.0, 200);
            let mut dg_redeem = Vec::new();
            encode_bytes_field(1, &id_bytes, &mut dg_redeem);
            assert_eq!(q_rpc(&ctx, &mut pilot.sess, "eve_public.dailygoal.api.RedeemRequest", dg_redeem).await.0, 200);
        }
    }

    pilot.sess.role = service_roles::ROLE_PLAYER;
    let _ = drain_notices(&mut rx);

    // 2. Negative authorization & validation checks.
    let mut unauth = SessionState::new(97_198);
    assert_eq!(q_rpc(&ctx, &mut unauth, "eve_public.entitlement.character.GetAllRequest", Vec::new()).await.0, 401);
    let mut bad_admin_grant = Vec::new();
    encode_bytes_field(1, &logo_ident_msg(cid, 602), &mut bad_admin_grant);
    assert_eq!(q_rpc(&ctx, &mut pilot.sess, "eve.entitlement.character.ship.admin.corplogo.GrantRequest", bad_admin_grant.clone()).await.0, 403);
    assert_eq!(q_rpc(&ctx, &mut pilot.sess, "eve_public.entitlement.character.ship.admin.alliancelogo.GrantRequest", bad_admin_grant.clone()).await.0, 403);
    assert_eq!(q_rpc(&ctx, &mut pilot.sess, "eve.entitlement.character.admin.GetAllRequest", Vec::new()).await.0, 403);
    assert_eq!(q_rpc(&ctx, &mut pilot.sess, "eve.entitlement.character.api.IsEntitledRequest", Vec::new()).await.0, 400);

    let mut other_query = Vec::new();
    encode_bytes_field(1, &logo_ident_msg(gm.char_id as u64, 602), &mut other_query);
    assert_eq!(q_rpc(&ctx, &mut pilot.sess, "eve.entitlement.character.api.IsEntitledRequest", other_query).await.0, 403);

    let mut unentitled_req = Vec::new();
    encode_bytes_field(1, &logo_ident_msg(cid, 602), &mut unentitled_req);
    assert_eq!(q_rpc(&ctx, &mut pilot.sess, "eve.entitlement.character.api.IsEntitledRequest", unentitled_req.clone()).await.0, 404);

    // 3. Purchase Paragon EverMark ship emblem via LPStoreMgr.TakeOfferForCharacter.
    let PyRep::List(offers) = rpc(&ctx, &mut pilot.sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(PARAGON_CORP_ID)])
        .await
        .expect("Paragon LP offers")
    else { panic!("expected offers list"); };
    let affordable = offers
        .iter()
        .filter(|o| {
            o.dict_get("offerID").and_then(|v| v.as_i64()).is_some_and(|oid| oid == 17_208 || oid >= 600_000)
                && o.dict_get("lpCost").and_then(|v| v.as_i64()).is_some_and(|c| c > 0 && c <= 5_000)
        })
        .min_by_key(|o| o.dict_get("lpCost").and_then(|v| v.as_i64()).unwrap_or(i64::MAX))
        .expect("affordable Paragon emblem offer");
    let lp_offer_id = affordable.dict_get("offerID").and_then(|v| v.as_i64()).unwrap();
    let take_rep = rpc(&ctx, &mut pilot.sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(PARAGON_CORP_ID), PyRep::Integer(lp_offer_id), PyRep::Integer(1)])
        .await
        .expect("buy Paragon emblem offer");
    assert_eq!(take_rep.dict_get("offerID").and_then(|v| v.as_i64()), Some(lp_offer_id));
    let lp_notices = drain_notices(&mut rx);
    assert!(
        has_notice(&lp_notices, "eve_public.entitlement.character.ship.corplogo.GrantedNotice")
            || has_notice(&lp_notices, "eve_public.entitlement.character.ship.alliancelogo.GrantedNotice")
    );

    // 4. Admin Grant/Revoke across eve & eve_public + IsEntitled + admin GetAll + public GetAll.
    assert_eq!(q_rpc(&ctx, &mut gm.sess, "eve.entitlement.character.ship.admin.corplogo.GrantRequest", bad_admin_grant.clone()).await.0, 200);
    let mut ally_grant = Vec::new();
    encode_bytes_field(1, &logo_ident_msg(cid, 621), &mut ally_grant);
    assert_eq!(q_rpc(&ctx, &mut gm.sess, "eve_public.entitlement.character.ship.admin.alliancelogo.GrantRequest", ally_grant.clone()).await.0, 200);
    let g_notices = drain_notices(&mut rx);
    assert!(has_notice(&g_notices, "eve_public.entitlement.character.ship.corplogo.GrantedNotice"));
    assert!(has_notice(&g_notices, "eve_public.entitlement.character.ship.alliancelogo.GrantedNotice"));

    let (ec1, et1, _) = q_rpc(&ctx, &mut pilot.sess, "eve.entitlement.character.api.IsEntitledRequest", unentitled_req.clone()).await;
    assert_eq!(ec1, 200);
    assert_eq!(et1, "eve.entitlement.character.api.IsEntitledResponse");
    let mut ally_check = Vec::new();
    encode_bytes_field(2, &logo_ident_msg(cid, 621), &mut ally_check);
    let (ec2, et2, _) = q_rpc(&ctx, &mut pilot.sess, "eve.entitlement.character.api.IsEntitledRequest", ally_check.clone()).await;
    assert_eq!(ec2, 200);
    assert_eq!(et2, "eve.entitlement.character.api.IsEntitledResponse");

    let (pc, pt, pb) = q_rpc(&ctx, &mut pilot.sess, "eve_public.entitlement.character.GetAllRequest", Vec::new()).await;
    assert_eq!(pc, 200);
    assert_eq!(pt, "eve_public.entitlement.character.GetAllResponse");
    assert_eq!(WireMessage::decode(&pb).unwrap().fields.len(), 3);

    let mut admin_all_req = Vec::new();
    encode_bytes_field(1, &seq_msg(cid), &mut admin_all_req);
    let (ac, at, ab) = q_rpc(&ctx, &mut gm.sess, "eve.entitlement.character.admin.GetAllRequest", admin_all_req).await;
    assert_eq!(ac, 200);
    assert_eq!(at, "eve.entitlement.character.admin.GetAllResponse");
    assert_eq!(WireMessage::decode(&ab).unwrap().fields.len(), 3);

    assert_eq!(q_rpc(&ctx, &mut gm.sess, "eve_public.entitlement.character.ship.admin.corplogo.RevokeRequest", bad_admin_grant).await.0, 200);
    assert_eq!(q_rpc(&ctx, &mut gm.sess, "eve_public.entitlement.character.ship.admin.alliancelogo.RevokeRequest", ally_grant).await.0, 200);
    let r_notices = drain_notices(&mut rx);
    assert!(has_notice(&r_notices, "eve_public.entitlement.character.ship.corplogo.RevokedNotice"));
    assert!(has_notice(&r_notices, "eve_public.entitlement.character.ship.alliancelogo.RevokedNotice"));

    assert_eq!(q_rpc(&ctx, &mut pilot.sess, "eve.entitlement.character.api.IsEntitledRequest", unentitled_req).await.0, 404);
    assert_eq!(q_rpc(&ctx, &mut pilot.sess, "eve.entitlement.character.api.IsEntitledRequest", ally_check).await.0, 404);
}

#[tokio::test]
async fn test_structures_fulcrum_alignment_services_live() {
    // parity: feat:structures.fulcrum-alignment-services
    eve_testkit::attest(&["feat:structures.fulcrum-alignment-services"]);

    let ctx = ServerContext::new();
    ctx.set_clock_paused(true);

    let mut pilot = common::gm_pilot_in_space(&ctx, 97_201, 97_200_001, 587, ZARZAKH_SYS).await;
    let mut target = common::gm_pilot_in_space(&ctx, 97_202, 97_200_002, 587, ZARZAKH_SYS).await;
    common::slash_cmd(&ctx, &mut pilot.sess, "/giveskill 24242 5").await;
    common::slash_cmd(&ctx, &mut pilot.sess, "/giveskill 3300 1").await;
    common::slash_cmd(&ctx, &mut pilot.sess, "/giveskill 3303 1").await;
    common::slash_cmd(&ctx, &mut pilot.sess, "/giveskill 3363 1").await;
    common::slash_cmd(&ctx, &mut pilot.sess, "/giveisk 100000000").await;

    let fulcrum_pos = ctx.sde.stations[&FULCRUM_STATION].position;
    let z_sys = ctx.systems.get_or_spawn(ZARZAKH_SYS);
    z_sys.set_ball_position(pilot.ship_id, fulcrum_pos).await;
    ctx.advance_secs(12);
    rpc(&ctx, &mut pilot.sess, "stationSvc", "Dock", &[PyRep::Integer(FULCRUM_STATION), PyRep::Integer(pilot.ship_id)]).await.unwrap();
    let laser_id = common::stock_ship_bay(&ctx, &mut pilot, 451, 1, InventoryFlag::HiSlot0 as i64).await;
    rpc(&ctx, &mut pilot.sess, "stationSvc", "Undock", &[]).await.unwrap();
    z_sys.set_ball_position(pilot.ship_id, fulcrum_pos + DVec3::new(100.0, 0.0, 0.0)).await;
    z_sys.set_ball_position(target.ship_id, fulcrum_pos + DVec3::new(600.0, 0.0, 0.0)).await;
    ctx.advance_secs(31);
    pilot.sess.role = service_roles::ROLE_PLAYER;
    target.sess.role = service_roles::ROLE_PLAYER;

    // 1. Empire militia character is denied docking at The Fulcrum.
    rpc(&ctx, &mut target.sess, "facWarMgr", "JoinFactionAsCharacter", &[PyRep::Integer(500_001)])
        .await
        .expect("enlist Amarr militia");
    let err_empire = rpc(&ctx, &mut target.sess, "stationSvc", "Dock", &[PyRep::Integer(FULCRUM_STATION), PyRep::Integer(target.ship_id)])
        .await
        .expect_err("Fulcrum refuses Empire militia");
    assert_eq!(err_empire.code, "FulcrumRefusesEmpireMilitia");

    // 2. Active weapons timer (60s) & Zarzakh retribution timer (900s) deny docking until both expire.
    rpc(&ctx, &mut pilot.sess, "dogmaIM", "AddTarget", &[PyRep::Integer(target.ship_id)]).await.unwrap();
    for _ in 0..10 {
        ctx.advance_secs(1);
        z_sys.step_manual(1.0).await.unwrap();
    }
    rpc(&ctx, &mut pilot.sess, "dogmaIM", "Activate", &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(target.ship_id)])
        .await
        .expect("activate weapon in Zarzakh");

    let err_wpn = rpc(&ctx, &mut pilot.sess, "stationSvc", "Dock", &[PyRep::Integer(FULCRUM_STATION), PyRep::Integer(pilot.ship_id)])
        .await
        .expect_err("Fulcrum refuses docking with active weapons timer");
    assert_eq!(err_wpn.code, "FulcrumRefusesTimerActive");

    ctx.advance_secs(65); // Weapons timer expired, Zarzakh retribution timer (900s) still active.
    let err_retrib = rpc(&ctx, &mut pilot.sess, "stationSvc", "Dock", &[PyRep::Integer(FULCRUM_STATION), PyRep::Integer(pilot.ship_id)])
        .await
        .expect_err("Fulcrum refuses docking with active Zarzakh retribution timer");
    assert_eq!(err_retrib.code, "FulcrumRefusesTimerActive");

    ctx.advance_secs(845); // All timers expired.

    // 3. Neutral character docks at The Fulcrum and accesses standard services, but is denied Jump Clone & Corp Office.
    rpc(&ctx, &mut pilot.sess, "stationSvc", "Dock", &[PyRep::Integer(FULCRUM_STATION), PyRep::Integer(pilot.ship_id)])
        .await
        .expect("Neutral pilot docks at Fulcrum");
    ctx.advance_secs(12);
    let _ = rpc(&ctx, &mut pilot.sess, "repairSvc", "GetRepairQuotes", &[]).await.expect("repairSvc");
    let _ = rpc(&ctx, &mut pilot.sess, "insuranceSvc", "GetInsurancePrices", &[]).await.expect("insuranceSvc");
    let _ = rpc(&ctx, &mut pilot.sess, "reprocessingSvc", "GetQuotes", &[]).await.expect("reprocessingSvc");
    let _ = rpc(&ctx, &mut pilot.sess, "invbroker", "Add", &[PyRep::Integer(laser_id), PyRep::Integer(pilot.ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect("fitting unfit");
    let _ = rpc(&ctx, &mut pilot.sess, "marketProxy", "GetOrders", &[PyRep::Integer(34)]).await.expect("marketProxy");
    let _ = rpc(&ctx, &mut pilot.sess, "facilityManager", "GetFacilities", &[]).await.expect("facilityManager");

    let err_clone = rpc(&ctx, &mut pilot.sess, "jumpCloneSvc", "InstallCloneInStation", &[])
        .await
        .expect_err("Neutral character denied Jump Clone at Fulcrum");
    assert_eq!(err_clone.code, "FulcrumCloneRequiresPirateEnlistment");

    let corp_rep = rpc(&ctx, &mut pilot.sess, "corpRegistry", "AddCorporation", &[PyRep::String("Fulcrum Aligned Corp".into()), PyRep::String("FLCR".into())])
        .await
        .expect("AddCorporation");
    let corp_id = corp_rep.as_i64().or(pilot.sess.corporation_id).expect("corp_id");
    pilot.sess.corporation_id = Some(corp_id);
    rpc(&ctx, &mut pilot.sess, "account", "GiveCash", &[PyRep::Integer(corp_id), PyRep::Real(50_000_000.0)]).await.unwrap();
    ctx.advance_secs(12);
    let err_office = rpc(&ctx, &mut pilot.sess, "officeManager", "RentOffice", &[PyRep::Integer(FULCRUM_STATION)])
        .await
        .expect_err("Neutral corp denied office rental at Fulcrum");
    assert_eq!(err_office.code, "FulcrumOfficeRequiresPirateEnlistment");

    rpc(&ctx, &mut pilot.sess, "stationSvc", "Undock", &[]).await.unwrap();
    let neutral_inertia = z_sys.get_ball_snapshot(pilot.ship_id).await.unwrap().unwrap().1.inertia;
    ctx.advance_secs(12);
    rpc(&ctx, &mut pilot.sess, "stationSvc", "Dock", &[PyRep::Integer(FULCRUM_STATION), PyRep::Integer(pilot.ship_id)]).await.unwrap();
    ctx.advance_secs(12);

    // 4. Enlist character + corp in Angel Cartel (500_010) -> Jump Clone, Corp Office, Hosted Pirate LP Stores & 0.80x inertia buff!
    rpc(&ctx, &mut pilot.sess, "facWarMgr", "JoinFactionAsCharacter", &[PyRep::Integer(500_010)]).await.unwrap();
    rpc(&ctx, &mut pilot.sess, "facWarMgr", "JoinFactionAsCorporation", &[PyRep::Integer(500_010)]).await.unwrap();
    ctx.advance_secs(12);

    let _ = rpc(&ctx, &mut pilot.sess, "jumpCloneSvc", "InstallCloneInStation", &[]).await.expect("pirate jump clone");
    let _ = rpc(&ctx, &mut pilot.sess, "officeManager", "RentOffice", &[PyRep::Integer(FULCRUM_STATION)]).await.expect("pirate corp office");

    let PyRep::List(angel_offers) = rpc(&ctx, &mut pilot.sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(ANGEL_LP_CORP)])
        .await
        .expect("Angel Cartel LP store at Fulcrum")
    else { panic!("expected Angel LP offers"); };
    assert_eq!(angel_offers[0].dict_get("corpID").and_then(|v| v.as_i64()), Some(ANGEL_LP_CORP));

    let PyRep::List(guristas_offers) = rpc(&ctx, &mut pilot.sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(GURISTAS_LP_CORP)])
        .await
        .expect("Guristas LP store at Fulcrum")
    else { panic!("expected Guristas LP offers"); };
    assert_eq!(guristas_offers[0].dict_get("corpID").and_then(|v| v.as_i64()), Some(GURISTAS_LP_CORP));

    rpc(&ctx, &mut pilot.sess, "stationSvc", "Undock", &[]).await.unwrap();
    let pirate_inertia = z_sys.get_ball_snapshot(pilot.ship_id).await.unwrap().unwrap().1.inertia;
    assert!((pirate_inertia - neutral_inertia * 0.80).abs() < 1e-4, "Deathless alignment must reduce inertia by 20%");
}
