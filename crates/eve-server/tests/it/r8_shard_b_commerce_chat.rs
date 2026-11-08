//! Round-8 Shard-B integration regression tests for `#475`, `#481`, `#484`, `#486`, and `#521`.

#![forbid(unsafe_code)]

use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

async fn call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

async fn send_quasar(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload: Vec<u8>,
) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"r8-shard-b-475".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload,
        }),
        external_origin: "r8b-test".into(),
        application_instance_uuid: b"r8b-app-inst".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = call(
        ctx,
        sess,
        "publicGatewaySvc",
        "SendRequest",
        &[PyRep::Bytes(req.encode_proto())],
    )
    .await
    .expect("publicGatewaySvc.SendRequest");
    if let PyRep::KeyVal(entries) = &rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                }
            }
        }
    }
    panic!("expected response_proto KeyVal, got {rep:?}");
}

fn encode_entitlement_id(holder: u32, asset_uuid: &[u8; 16], group_name: &str) -> Vec<u8> {
    let (mut ch, mut h, mut a, mut g, mut id) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, holder as u64, &mut ch);
    encode_bytes_field(1, &ch, &mut h);
    encode_bytes_field(1, asset_uuid, &mut a);
    encode_string_field(1, group_name, &mut g);
    encode_bytes_field(1, &h, &mut id);
    encode_bytes_field(2, &a, &mut id);
    encode_bytes_field(3, &g, &mut id);
    id
}

#[tokio::test]
async fn r8b_475_quasar_entitlement_redeem_atomic_rmw_and_rollback() {
    let ctx = ServerContext::new();
    let holder_id = 90_000_001_i64;
    let gm_id = 90_000_002_i64;

    let mut gm_sess = SessionState::new(8101);
    gm_sess.user_id = Some(1);
    gm_sess.character_id = Some(gm_id);
    gm_sess.role = service_roles::ROLE_GMH | service_roles::ROLE_ADMIN;

    let (mut t_msg, mut st_msg, mut st_u, mut unit, mut create_req) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, 34, &mut t_msg);
    encode_bytes_field(1, &t_msg, &mut st_msg);
    encode_varint_field(2, 500, &mut st_msg);
    encode_bytes_field(1, &st_msg, &mut st_u);
    encode_bytes_field(8, &st_u, &mut unit);
    encode_bytes_field(4, &unit, &mut create_req);
    let create_rsp = send_quasar(
        &ctx,
        &mut gm_sess,
        "eve.assetholding.asset.api.CreateRequest",
        create_req,
    )
    .await;
    assert_eq!(create_rsp.status_code, STATUS_OK);
    let rsp_val = create_rsp.payload.expect("CreateResponse payload").value;
    let asset_uuid: [u8; 16] = rsp_val[rsp_val.len() - 16..].try_into().unwrap();

    let ent_id = encode_entitlement_id(holder_id as u32, &asset_uuid, "r8b-pack");
    let (mut entry, mut dd_req) = (Vec::new(), Vec::new());
    encode_bytes_field(2, &ent_id, &mut entry);
    encode_varint_field(10, 2, &mut entry);
    encode_bytes_field(2, &entry, &mut dd_req);
    assert_eq!(
        send_quasar(
            &ctx,
            &mut gm_sess,
            "eve.assetholding.entitlement.api.DirectDeliveryRequest",
            dd_req,
        )
        .await
        .status_code,
        STATUS_OK
    );

    let holder_station = ctx.db.with_snapshot(|s| {
        s.characters[&holder_id]
            .station_id
            .unwrap_or(s.characters[&holder_id].home_station_id)
    });
    let mut holder_sess = SessionState::new(8102);
    holder_sess.user_id = Some(1);
    holder_sess.character_id = Some(holder_id);

    let mut redeem_req = Vec::new();
    encode_bytes_field(1, &ent_id, &mut redeem_req);

    // Trigger atomic error mid-redeem by removing holder character record; entitlement must stay unredeemed.
    let saved_char = ctx.db.with_mut_snapshot(|s| s.characters.remove(&holder_id).unwrap());
    let fail_rsp = send_quasar(
        &ctx,
        &mut holder_sess,
        "eve_public.assetholding.entitlement.api.RedeemRequest",
        redeem_req.clone(),
    )
    .await;
    assert_ne!(fail_rsp.status_code, STATUS_OK);

    ctx.db.with_mut_snapshot(|s| {
        s.characters.insert(holder_id, saved_char);
    });
    let before_trit: i64 = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|i| i.owner_id == holder_id && i.type_id == 34 && i.location_id == holder_station)
            .map(|i| i.quantity as i64)
            .sum()
    });
    let ok_rsp = send_quasar(
        &ctx,
        &mut holder_sess,
        "eve_public.assetholding.entitlement.api.RedeemRequest",
        redeem_req.clone(),
    )
    .await;
    assert_eq!(ok_rsp.status_code, STATUS_OK);
    let after_trit: i64 = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|i| i.owner_id == holder_id && i.type_id == 34 && i.location_id == holder_station)
            .map(|i| i.quantity as i64)
            .sum()
    });
    assert_eq!(after_trit - before_trit, 1_000);
    assert_ne!(
        send_quasar(
            &ctx,
            &mut holder_sess,
            "eve_public.assetholding.entitlement.api.RedeemRequest",
            redeem_req,
        )
        .await
        .status_code,
        STATUS_OK
    );
}

#[tokio::test]
async fn r8b_481_521_global_mute_persistence_and_chat_channel_acl_security() {
    let db_path = std::env::temp_dir().join(format!(
        "evomu_r8b_481_{}_{}.sqlite",
        std::process::id(),
        90_481_521_u64
    ));
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", db_path.display()));
    }
    WorldRepository::with_demo_seed()
        .save_to_sqlite(&db_path)
        .expect("seed sqlite");
    let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
    let ctx = ServerContext::with_repository(Arc::clone(&db));

    let (c1, c2, corp1, corp2) = (90_000_001_i64, 90_000_002_i64, 98_481_001_i64, 98_481_002_i64);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&c1).unwrap().corporation_id = corp1;
        s.characters.get_mut(&c1).unwrap().solar_system_id = 30_000_142;
        s.characters.get_mut(&c2).unwrap().corporation_id = corp2;
        s.characters.get_mut(&c2).unwrap().solar_system_id = 30_002_187;
        s.corporations.insert(
            corp1,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp1;
                __corp.name = "R8B Corp One".into();
                __corp.ticker = "R8B1".into();
                __corp.ceo_id = c1;
                __corp.members = HashSet::from([c1]);
                __corp.member_roles = HashMap::from([(c1, eve_db::corp_roles::ROLE_DIRECTOR)]);
                __corp },
        );
        s.corporations.insert(
            corp2,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp2;
                __corp.name = "R8B Corp Two".into();
                __corp.ticker = "R8B2".into();
                __corp.ceo_id = c2;
                __corp.members = HashSet::from([c2]);
                __corp },
        );
    });

    let mut s1 = SessionState::new(8201);
    s1.user_id = Some(1);
    s1.character_id = Some(c1);
    s1.corporation_id = Some(corp1);
    s1.solar_system_id = Some(30_000_142);

    let mut s2 = SessionState::new(8202);
    s2.user_id = Some(2);
    s2.character_id = Some(c2);
    s2.corporation_id = Some(corp2);
    s2.solar_system_id = Some(30_002_187);

    let jita_local = "solarsystemid2:30000142";
    let amarr_local = "solarsystemid2:30002187";
    let corp1_chan = format!("corpid:{corp1}");
    call(&ctx, &mut s1, "LSC", "JoinChannel", &[PyRep::String(jita_local.into())])
        .await
        .expect("join own local");
    ctx.db.with_snapshot(|s| {
        let ch = &s.chat_channels[jita_local];
        assert_eq!(ch.owner_id, None);
        assert!(ch.operators.is_empty());
    });
    assert_eq!(
        call(&ctx, &mut s1, "XmppChatMgr", "GrantChannelOwnership", &[PyRep::String(jita_local.into()), PyRep::Integer(c2)])
            .await.unwrap_err().code,
        "NotChannelOwner"
    );
    assert_eq!(
        call(&ctx, &mut s1, "LSC", "AccessControl", &[PyRep::String(jita_local.into()), PyRep::Integer(c2), PyRep::Integer(2)])
            .await.unwrap_err().code,
        "NotChannelOperator"
    );
    assert_eq!(
        call(&ctx, &mut s2, "LSC", "JoinChannel", &[PyRep::String(jita_local.into())])
            .await.unwrap_err().code,
        "ChannelAccessDenied"
    );
    assert_eq!(
        call(&ctx, &mut s2, "LSC", "JoinChannel", &[PyRep::String(corp1_chan.clone())])
            .await.unwrap_err().code,
        "ChannelAccessDenied"
    );

    call(&ctx, &mut s1, "LSC", "JoinChannel", &[PyRep::String(corp1_chan.clone())])
        .await
        .expect("join own corp");
    call(&ctx, &mut s1, "XmppChatMgr", "SetMotd", &[PyRep::String(corp1_chan), PyRep::String("Fleet Up".into())])
        .await
        .expect("corp CEO/director sets MOTD via dynamic role check");

    let create_priv = call(&ctx, &mut s1, "XmppChatMgr", "CreatePrivateChannel", &[PyRep::Integer(c1)])
        .await
        .expect("create private channel");
    let priv_id = create_priv.dict_get("channelID").and_then(|v| v.as_str()).unwrap().to_string();
    assert_eq!(
        call(&ctx, &mut s2, "LSC", "JoinChannel", &[PyRep::String(priv_id.clone())]).await.unwrap_err().code,
        "ChannelAccessDenied"
    );
    assert_eq!(
        call(&ctx, &mut s2, "LSC", "GetMembers", &[PyRep::String(priv_id)]).await.unwrap_err().code,
        "ChannelAccessDenied"
    );

    let mut gm = SessionState::new(8203);
    gm.user_id = Some(99);
    gm.character_id = Some(c1);
    gm.role = service_roles::ROLE_GMH;
    call(
        &ctx, &mut gm, "XmppChatMgr", "GMMute",
        &[PyRep::Integer(c2), PyRep::String("global".into()), PyRep::Integer(600), PyRep::String("global spam".into())],
    )
    .await
    .expect("GM global mute");

    call(&ctx, &mut s2, "LSC", "JoinChannel", &[PyRep::String(amarr_local.into())]).await.expect("c2 join own local");
    assert_eq!(
        call(&ctx, &mut s2, "LSC", "SendMessage", &[PyRep::String(amarr_local.into()), PyRep::String("spam".into())])
            .await.unwrap_err().code,
        "MutedInChannel"
    );

    drop(ctx);
    drop(db);
    let reloaded_db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    reloaded_db.with_snapshot(|s| {
        assert!(!s.chat_channels.contains_key("global"));
        assert!(s.chat_channel_acl["global"].muted_until.contains_key(&c2));
    });
    let ctx2 = ServerContext::with_repository(reloaded_db);
    assert_eq!(
        call(&ctx2, &mut s2, "LSC", "SendMessage", &[PyRep::String(amarr_local.into()), PyRep::String("still muted".into())])
            .await.unwrap_err().code,
        "MutedInChannel"
    );
    drop(ctx2);
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", db_path.display()));
    }
}

#[tokio::test]
async fn r8b_484_486_lp_bpc_minting_and_lootall_range_mtu_alsc_locks() {
    let ctx = ServerContext::new();
    let (char_id, rival_id, station_id, sys_id, lp_corp) =
        (90_000_001_i64, 90_000_002_i64, 60_003_760_i64, 30_000_142_u32, 1_000_035_i64);
    let bp_offer = ctx.db.with_snapshot(|s| {
        eve_server::services::lp_store_catalog::get_corp_lp_offers_with_sde(&ctx.sde, s, lp_corp)
            .into_iter()
            .find(|o| {
                ctx.sde
                    .types
                    .get(&o.type_id)
                    .is_some_and(|t| t.category_id == 9)
            })
            .expect("Caldari Navy blueprint LP offer")
    });
    let bp_type_id = bp_offer.type_id;

    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(station_id);
        ch.solar_system_id = sys_id;
        ch.balance = IskCents::from_isk(5_000_000_000);
        ch.loyalty_points.insert(lp_corp, 50_000_000);
        for (idx, &(req_tid, req_qty)) in bp_offer.req_items.iter().enumerate() {
            let iid = 880_484_100 + idx as i64;
            s.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: req_tid,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: req_qty.max(1) * 2,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    let mut docked_sess = SessionState::new(8301);
    docked_sess.user_id = Some(1);
    docked_sess.character_id = Some(char_id);
    docked_sess.station_id = Some(station_id);
    docked_sess.solar_system_id = Some(sys_id);

    let take_rep = call(
        &ctx,
        &mut docked_sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(lp_corp), PyRep::Integer(bp_offer.offer_id), PyRep::Integer(1)],
    )
    .await
    .expect("TakeOfferForCharacter blueprint");
    let minted_bp_id = take_rep
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .or_else(|| {
            ctx.db.with_snapshot(|s| {
                s.items
                    .values()
                    .find(|i| i.owner_id == char_id && i.type_id == bp_type_id && i.location_id == station_id)
                    .map(|i| i.item_id)
            })
        })
        .expect("minted blueprint item");
    ctx.db.with_snapshot(|s| {
        let bp = &s.items[&minted_bp_id];
        assert!(bp.is_blueprint_copy && bp.blueprint_runs_remaining.unwrap_or(0) >= 1);
    });

    let (ship_id, wreck_id, foreign_mtu_id, alsc_cont_id, locked_id, unlocked_id, wreck_bpc_id) = (
        880_486_001, 880_486_002, 880_486_003, 880_486_004, 880_486_005, 880_486_006, 880_486_007,
    );
    ctx.db.with_mut_snapshot(|s| {
        for (id, tid, owner, loc, flag, qty, sing, is_bpc, bpc_runs) in [
            (ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true, false, None),
            (wreck_id, 26_561, char_id, sys_id as i64, InventoryFlag::None, 1, true, false, None),
            (foreign_mtu_id, 33_475, rival_id, sys_id as i64, InventoryFlag::None, 1, true, false, None),
            (alsc_cont_id, 17_366, char_id, sys_id as i64, InventoryFlag::None, 1, true, false, None),
            (locked_id, 34, char_id, alsc_cont_id, InventoryFlag::Locked, 50, false, false, None),
            (unlocked_id, 35, char_id, alsc_cont_id, InventoryFlag::Cargo, 25, false, false, None),
            (wreck_bpc_id, bp_type_id, char_id, wreck_id, InventoryFlag::Cargo, 1, true, true, Some(5)),
        ] {
            s.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: owner,
                    location_id: loc,
                    flag,
                    quantity: qty,
                    singleton: sing,
                    version: 1,
                    is_blueprint_copy: is_bpc,
                    blueprint_runs_remaining: bpc_runs,
                    ..Default::default()
                },
            );
        }
    });

    assert_eq!(
        call(&ctx, &mut docked_sess, "invbroker", "LootAll", &[PyRep::Integer(wreck_id)])
            .await
            .unwrap_err()
            .code,
        "CantDoThatWhileDocked"
    );

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, owner, pos) in [
        (ship_id, 587, char_id, DVec3::ZERO),
        (wreck_id, 26_561, char_id, DVec3::new(10_000.0, 0.0, 0.0)),
        (foreign_mtu_id, 33_475, rival_id, DVec3::new(500.0, 0.0, 0.0)),
        (alsc_cont_id, 17_366, char_id, DVec3::new(600.0, 0.0, 0.0)),
    ] {
        sys.spawn_ball_with_meta(
            Ball {
                id,
                type_id: tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: owner,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.active_ship_id = ship_id;
    });
    let mut space_sess = SessionState::new(8302);
    space_sess.user_id = Some(1);
    space_sess.character_id = Some(char_id);
    space_sess.solar_system_id = Some(sys_id);
    space_sess.ship_id = Some(ship_id);

    assert_eq!(
        call(&ctx, &mut space_sess, "invbroker", "LootAll", &[PyRep::Integer(wreck_id)])
            .await
            .unwrap_err()
            .code,
        "OutOfRange"
    );
    let mtu_err = call(&ctx, &mut space_sess, "invbroker", "LootAll", &[PyRep::Integer(foreign_mtu_id)])
        .await
        .unwrap_err();
    // rpc-lint: allow error code check
    assert_eq!(mtu_err.code, "AccessDenied");

    call(&ctx, &mut space_sess, "invbroker", "LootAll", &[PyRep::Integer(alsc_cont_id)])
        .await
        .expect("LootAll on owned ALSC container");
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&locked_id].location_id, alsc_cont_id);
        assert_eq!(s.items[&unlocked_id].location_id, ship_id);
    });

    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.get_mut(&wreck_id).unwrap().position = DVec3::new(1_000.0, 0.0, 0.0);
    }
    call(&ctx, &mut space_sess, "invbroker", "LootAll", &[PyRep::Integer(wreck_id)])
        .await
        .expect("LootAll within 2500m");
    ctx.db.with_snapshot(|s| {
        let bp = &s.items[&wreck_bpc_id];
        assert_eq!(bp.location_id, ship_id);
        assert!(bp.is_blueprint_copy && bp.blueprint_runs_remaining == Some(5));
    });
}
