//! Integration tests for live V24.01 playtesting fixes (`#315`, `#316`, `#319`, `#320`):
//! - `#315`: `beyonce.CmdWarpToStuff` emits V24.01 5-arg `WarpTo(ball_id, x, y, z, warp_speed_x10)`
//! - `#316`: Bound `invbroker.ListByFlags(flags)` filters container items by requested inventory flags
//! - `#319`/`#320`: `eve_public.dailygoal.api.GetAllCurrentRequest` / `GetRequest` returns a valid
//!   `CATEGORY_DAILY_BONUS` (`category = 2`) daily bonus job definition

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, DynamicValue, EvePublicRequest, EvePublicResponse, GoogleAny,
    GoogleTimestamp, QuasarDescriptorRegistry, QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT: i64 = 90_000_001;
const JITA: u32 = 30_000_142;

async fn login_session(ctx: &ServerContext, sess_id: u64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(sess_id);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(PILOT)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

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
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"live-playtest-01".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"live-playtest-app".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &kw,
            None,
        )
        .await
        .expect("publicGatewaySvc.SendRequest");
    extract_response_proto(&rep)
}

#[tokio::test]
async fn test_315_cmd_warp_to_stuff_emits_v24_five_arg_warp_to() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = login_session(&ctx, 31501).await;
    let undock = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(kv) = &undock else {
        panic!("Undock must return KeyVal");
    };
    let beyonce = kv["beyonceBoundID"].as_i64().unwrap() as u64;
    let ship_id = sess.ship_id.expect("active ship");
    let jita_handle = ctx.systems.get_or_spawn(JITA);
    let _ = jita_handle.drain_observer_updates(ship_id).await;

    let gate_id = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == JITA)
        .map(|j| j.stargate_id)
        .expect("Jita stargate");

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce),
        "CmdWarpToStuff",
        &[
            PyRep::String("item".into()),
            PyRep::Integer(gate_id),
            PyRep::Real(0.0),
        ],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToStuff");

    let (_tick, _evs, updates) = jita_handle
        .drain_observer_updates(ship_id)
        .await
        .expect("drain observer updates");
    let PyRep::List(entries) = updates else {
        panic!("DoDestinyUpdate payload must be a list");
    };
    let warp_entry = entries
        .iter()
        .find_map(|entry| {
            let PyRep::Tuple(outer) = entry else { return None };
            let PyRep::Tuple(inner) = &outer[1] else { return None };
            if inner[0].as_str() == Some("WarpTo") {
                Some(inner[1].clone())
            } else {
                None
            }
        })
        .expect("WarpTo action must be emitted");
    let PyRep::Tuple(warp_args) = warp_entry else {
        panic!("WarpTo args must be a tuple");
    };
    assert_eq!(
        warp_args.len(),
        6,
        "V24.01 Destiny.Ballpark.WarpTo expects 6 args (ball_id, x, y, z, distance, warp_speed_milli_au_s), got {:?}",
        warp_args
    );
    assert_eq!(warp_args[0], PyRep::Integer(ship_id));
    assert!(matches!(warp_args[1], PyRep::Real(_)));
    assert!(matches!(warp_args[2], PyRep::Real(_)));
    assert!(matches!(warp_args[3], PyRep::Real(_)));
    assert_eq!(warp_args[4], PyRep::Integer(0));
    assert!(matches!(warp_args[5], PyRep::Integer(ws) if ws >= 1000));
}

#[tokio::test]
async fn test_316_invbroker_list_by_flags_filters_container_items() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = login_session(&ctx, 31601).await;
    let ship_id = sess.ship_id.expect("active ship");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            990_316_001,
            InventoryItemRecord {
                item_id: 990_316_001,
                type_id: 34, // Tritanium
                owner_id: PILOT,
                location_id: ship_id,
                flag: InventoryFlag::Cargo, // 5
                quantity: 100,
                singleton: false,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            990_316_002,
            InventoryItemRecord {
                item_id: 990_316_002,
                type_id: 35, // Pyerite
                owner_id: PILOT,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot0, // 11
                quantity: 1,
                singleton: true,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            990_316_003,
            InventoryItemRecord {
                item_id: 990_316_003,
                type_id: 36, // Mexallon
                owner_id: PILOT,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0, // 19
                quantity: 1,
                singleton: true,
                ..InventoryItemRecord::default()
            },
        );
    });

    let inv_bid = ctx.bind_object(&sess, "invbroker", ship_id).expect("bind ship invbroker");
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(inv_bid),
            "ListByFlags",
            &[PyRep::List(vec![PyRep::Integer(11), PyRep::Integer(19)])],
            &kw,
            None,
        )
        .await
        .expect("invbroker.ListByFlags");

    let PyRep::ObjectEx { list: rows, .. } = res else {
        panic!("ListByFlags must return CRowset ObjectEx, got {:?}", res);
    };
    assert_eq!(rows.len(), 2, "Expected 2 items matching flags [11, 19], got {}", rows.len());

    let empty_res = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(inv_bid),
            "ListByFlags",
            &[PyRep::List(Vec::new())],
            &kw,
            None,
        )
        .await
        .expect("invbroker.ListByFlags([])");
    let PyRep::ObjectEx { list: empty_rows, .. } = empty_res else {
        panic!("ListByFlags([]) must return CRowset ObjectEx");
    };
    assert!(empty_rows.is_empty(), "ListByFlags([]) must return 0 rows");
}

#[tokio::test]
async fn test_319_320_daily_goals_includes_daily_bonus_job() {
    let ctx = ServerContext::new();
    let mut sess = login_session(&ctx, 31901).await;
    let reg = QuasarDescriptorRegistry::open_default().expect("descriptor registry");

    let all_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.GetAllCurrentRequest", Vec::new()).await;
    assert_eq!(all_rsp.status_code, STATUS_OK);
    let wm = WireMessage::decode(&all_rsp.payload.unwrap().value).expect("decode GetAllCurrentResponse");

    let mut found_daily_bonus = false;
    for f in wm.fields.iter().filter(|f| f.field_number == 1) {
        let Some(goal_bytes) = f.value.as_bytes() else {
            continue;
        };
        let goal_dyn = reg
            .decode_dynamic(
                "eve_public.dailygoal.api.GetAllCurrentResponse.Goal",
                goal_bytes,
            )
            .expect("decode GetAllCurrentResponse.Goal");
        let Some(DynamicValue::Message(id_msg)) = goal_dyn.get(1) else {
            continue;
        };
        let Some(DynamicValue::Bytes(uuid_bytes)) = id_msg.get(1) else {
            continue;
        };
        let mut id_encoded = Vec::new();
        encode_bytes_field(1, uuid_bytes, &mut id_encoded);
        let get_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.dailygoal.api.GetRequest",
            {
                let mut req = Vec::new();
                encode_bytes_field(1, &id_encoded, &mut req);
                req
            },
        )
        .await;
        assert_eq!(get_rsp.status_code, STATUS_OK);
        let get_dyn = reg
            .decode_dynamic("eve_public.dailygoal.api.GetResponse", &get_rsp.payload.unwrap().value)
            .expect("decode GetResponse");
        if let Some(DynamicValue::Message(attrs)) = get_dyn.get(1) {
            if matches!(
                attrs.get(5),
                Some(
                    DynamicValue::Enum(2)
                        | DynamicValue::Int32(2)
                        | DynamicValue::Uint32(2)
                        | DynamicValue::Int64(2)
                        | DynamicValue::Uint64(2)
                )
            ) {
                found_daily_bonus = true;
                assert!(
                    attrs.get(6).is_some(),
                    "CATEGORY_DAILY_BONUS goal must include contribution_configuration (complete_daily_goal)"
                );
            }
        }
    }
    assert!(
        found_daily_bonus,
        "GetAllCurrentRequest must return a CATEGORY_DAILY_BONUS (category=2) goal so daily_bonus_job is not None"
    );
}

#[tokio::test]
async fn bounty_proxy_returns_zero_entries_for_requested_unlisted_targets() {
    let ctx = ServerContext::new();
    let mut sess = login_session(&ctx, 77).await;
    let kw = HashMap::new();

    let nonzero_id = 90_000_077_i64;
    let unlisted_id = 90_000_001_i64;
    let explicit_zero_id = 90_000_078_i64;
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.player_bounty_pool.insert(nonzero_id, 500_000.0);
        rt.player_bounty_pool.insert(explicit_zero_id, 0.0);
    });

    for req_arg in [
        PyRep::Set(vec![
            PyRep::Integer(nonzero_id),
            PyRep::Integer(unlisted_id),
            PyRep::Integer(explicit_zero_id),
        ]),
        PyRep::PySet(vec![
            PyRep::Integer(nonzero_id),
            PyRep::Integer(unlisted_id),
            PyRep::Integer(explicit_zero_id),
        ]),
        PyRep::Tuple(vec![
            PyRep::Integer(nonzero_id),
            PyRep::Integer(unlisted_id),
            PyRep::Integer(explicit_zero_id),
        ]),
        PyRep::List(vec![
            PyRep::Integer(nonzero_id),
            PyRep::Integer(unlisted_id),
            PyRep::Integer(explicit_zero_id),
        ]),
    ] {
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "bountyProxy",
                None,
                "GetBounties",
                &[req_arg],
                &kw,
                None,
            )
            .await
            .unwrap();
        let PyRep::List(rows) = res else {
            panic!("expected List from GetBounties");
        };
        assert_eq!(rows.len(), 3, "every requested ID must be returned");
        let mut by_id = HashMap::new();
        for entry in rows {
            let PyRep::Tuple(pair) = entry else {
                panic!("expected (targetID, KeyVal) tuple");
            };
            let tid = pair[0].as_i64().unwrap();
            let amt = pair[1].dict_get("bounty").and_then(|v| v.as_f64()).unwrap();
            assert_eq!(pair[1].dict_get("bountyISK").and_then(|v| v.as_f64()), Some(amt));
            assert_eq!(pair[1].dict_get("targetID").and_then(|v| v.as_i64()), Some(tid));
            by_id.insert(tid, amt);
        }
        assert_eq!(by_id.get(&nonzero_id), Some(&500_000.0));
        assert_eq!(by_id.get(&unlisted_id), Some(&0.0));
        assert_eq!(by_id.get(&explicit_zero_id), Some(&0.0));
    }

    // Empty request returns only positive entries
    let empty_res = ctx
        .dispatch_rpc(&mut sess, "bountyProxy", None, "GetBounties", &[], &kw, None)
        .await
        .unwrap();
    let PyRep::List(pos_rows) = empty_res else {
        panic!("expected List");
    };
    assert_eq!(pos_rows.len(), 1);

    // GetBountiesAndKillRights also includes requested 0.0 / unlisted IDs
    let combined = ctx
        .dispatch_rpc(
            &mut sess,
            "bountyProxy",
            None,
            "GetBountiesAndKillRights",
            &[
                PyRep::Set(vec![
                    PyRep::Integer(nonzero_id),
                    PyRep::Integer(unlisted_id),
                ]),
                PyRep::Set(vec![PyRep::Integer(unlisted_id)]),
            ],
            &kw,
            None,
        )
        .await
        .unwrap();
    let PyRep::Tuple(parts) = combined else {
        panic!("expected (bounties, killRights) tuple");
    };
    let PyRep::List(bounties) = &parts[0] else {
        panic!("expected bounty list");
    };
    assert_eq!(bounties.len(), 2);
}

#[tokio::test]
async fn on_item_change_emitters_include_owner_group_category_and_singleton() {
    let ctx = ServerContext::new();
    let mut sess = login_session(&ctx, 78).await;
    let kw = HashMap::new();
    let char_id = PILOT;
    let station_id = 60_003_760_i64;
    let trash_ship_id = 99_668_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            trash_ship_id,
            InventoryItemRecord {
                item_id: trash_ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(char_id);

    // TrashItems emitter
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "TrashItems",
        &[PyRep::List(vec![PyRep::Integer(trash_ship_id)]), PyRep::Integer(station_id)],
        &kw,
        None,
    )
    .await
    .unwrap();
    let trash_notifs = ctx.drain_notifications_for_char(char_id);
    let trash_ic = trash_notifs
        .iter()
        .find(|n| n.event == "OnItemChange")
        .expect("OnItemChange on TrashItems");
    assert_eq!(trash_ic.args[0].dict_get("ownerID").and_then(|v| v.as_i64()), Some(char_id));
    assert!(trash_ic.args[0].dict_get("groupID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert_eq!(trash_ic.args[0].dict_get("categoryID").and_then(|v| v.as_i64()), Some(6));
    assert_eq!(trash_ic.args[0].dict_get("quantity").and_then(|v| v.as_i64()), Some(-1));

    // Mission item reward emitter
    eve_server::services::agents_mission_rewards::emit_mission_item_reward_notification(
        &ctx,
        char_id,
        eve_server::services::agents_mission_rewards::MintedMissionItemReward {
            item_id: 99_668_002,
            type_id: 587,
            quantity: 1,
            station_id,
        },
    );
    let reward_notifs = ctx.drain_notifications_for_char(char_id);
    let reward_ic = reward_notifs
        .iter()
        .find(|n| n.event == "OnItemChange")
        .expect("OnItemChange on mission reward");
    assert_eq!(reward_ic.args[0].dict_get("ownerID").and_then(|v| v.as_i64()), Some(char_id));
    assert!(reward_ic.args[0].dict_get("groupID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert_eq!(reward_ic.args[0].dict_get("categoryID").and_then(|v| v.as_i64()), Some(6));
    assert_eq!(reward_ic.args[0].dict_get("quantity").and_then(|v| v.as_i64()), Some(-1));
}

/// Regression test for `#954` & `#789`: verifies over the real TCP `machoNet` client path that:
/// 1. `marketProxy.GetOrders` / `GetPlexOrders` wrap results in V24's whitelisted classic class
///    `carbon.common.script.net.objectCaching.CachedMethodCallResult` via `Op_PyObject` (`0x17`),
///    never emitting unimportable bare `objectCaching.*` on the wire.
/// 2. Subsequent `GetOrders` after `PlaceBuyOrder` advances `[timestamp, crc32]` so V24
///    `marketsvc.RefreshOrderCache` updates `volRemaining` instead of discarding fresh rows.
/// 3. `objectCaching.GetCachableObject` & `machoNet.GetCachedObject` emit V24's whitelisted
///    `carbon.common.script.net.objectCaching.CachedObject` via `Op_PyObject` (`0x17`).
#[tokio::test]
async fn test_954_789_machonet_market_orders_and_cached_objects_use_v24_object_caching_classes() {
    use eve_db::{IskCents, MarketOrderRecord};
    use eve_machonet::{
        placebo_crypto_hash, CryptoChallengePacket, CryptoHandshakeResult, CryptoRequestPacket,
        CryptoServerHandshake, MachoAddress, MachoPacket, MachoPacketType, MACHO_PROXY_NODE_ID,
    };
    use eve_proto::marshal::{
        self, V24_CACHED_METHOD_CALL_RESULT_CLASS, V24_CACHED_OBJECT_CLASS,
    };
    use eve_proto::MarshalOpcode;
    use eve_server::macho_listener::{handle_macho_client, read_framed_pyrep, write_framed_pyrep};
    use std::sync::Arc;
    use tokio::io::AsyncReadExt;
    use tokio::net::{TcpListener, TcpStream};

    let ctx = Arc::new(ServerContext::new());
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.type_id != 34 && o.type_id != 44_992);
        snap.market_orders.insert(
            954_001,
            MarketOrderRecord {
                order_id: 954_001,
                owner_id: 90_000_002,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: false,
                price: IskCents(204), // 2.04 ISK
                vol_entered: 1_962,
                vol_remaining: 1_962,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            954_002,
            MarketOrderRecord {
                order_id: 954_002,
                owner_id: 90_000_002,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 44_992,
                is_bid: false,
                price: IskCents::from_isk(5_000_000),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind macho");
    let addr = listener.local_addr().unwrap();
    let ctx_server = Arc::clone(&ctx);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let c = Arc::clone(&ctx_server);
            tokio::spawn(async move {
                let _ = handle_macho_client(stream, c).await;
            });
        }
    });

    let mut s = TcpStream::connect(addr).await.unwrap();
    let _hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    let client_ver = PyRep::Tuple(vec![
        PyRep::Integer(170_472),
        PyRep::Integer(496),
        PyRep::Integer(0),
        PyRep::Real(24.01),
        PyRep::Integer(3_624_425),
        PyRep::ascii_bytes("V24.01@ccp"),
    ]);
    write_framed_pyrep(&mut s, &client_ver).await.unwrap();
    write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep())
        .await
        .unwrap();
    assert_eq!(read_framed_pyrep(&mut s).await.unwrap().unwrap().as_str(), Some("OK CC"));

    let challenge = CryptoChallengePacket::new_password_login("demo", "demo-secret");
    write_framed_pyrep(&mut s, &challenge.to_pyrep()).await.unwrap();
    assert_eq!(read_framed_pyrep(&mut s).await.unwrap().unwrap(), PyRep::Integer(2));
    let srv_hs_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    let srv_hs = CryptoServerHandshake::from_pyrep(&srv_hs_rep).expect("CryptoServerHandshake");
    let hs_res = CryptoHandshakeResult {
        challenge_response_hash: placebo_crypto_hash(&srv_hs.server_challenge),
        func_output: Vec::new(),
        func_result: PyRep::Integer(0),
    };
    write_framed_pyrep(&mut s, &hs_res.to_pyrep()).await.unwrap();
    let _ack = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    let init_pkt =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    let MachoAddress::Client { client_id, .. } = init_pkt.destination else {
        panic!("expected Client destination on SessionInitialStateNotification");
    };
    let _sess_chg0 = read_framed_pyrep(&mut s).await.unwrap().unwrap();

    let send_call = |call_id: u64,
                     service: &str,
                     method: &str,
                     args: Vec<PyRep>,
                     kwargs: Vec<(PyRep, PyRep)>| {
        let inner_tuple = PyRep::Tuple(vec![
            PyRep::ascii_bytes(method),
            PyRep::Tuple(args),
            PyRep::Dict(kwargs),
        ]);
        MachoPacket {
            packet_type: MachoPacketType::CallReq,
            source: MachoAddress::Client {
                client_id,
                call_id,
                service: None,
            },
            destination: MachoAddress::Node {
                node_id: MACHO_PROXY_NODE_ID,
                service: Some(service.into()),
                call_id: 0,
            },
            user_id: Some(1001),
            payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
                PyRep::Integer(0),
                PyRep::SubStream(Box::new(inner_tuple)),
            ])]),
            named_payload: HashMap::new(),
        }
    };

    // SelectCharacterID(90_000_001)
    write_framed_pyrep(
        &mut s,
        &send_call(
            1,
            "charUnboundMgr",
            "SelectCharacterID",
            vec![PyRep::Integer(PILOT)],
            vec![],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    let _sel_sess = MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert_eq!(_sel_sess.packet_type, MachoPacketType::SessionChangeNotification);
    let sel_rsp = MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert_eq!(sel_rsp.packet_type, MachoPacketType::CallRsp);

    // Helper: reads raw length-prefixed wire frames until CallRsp, returning (raw_frame_bytes, CallRsp).
    async fn read_call_rsp_wire(stream: &mut TcpStream) -> (Vec<u8>, MachoPacket) {
        loop {
            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).await.expect("frame len");
            let frame_len = u32::from_le_bytes(len_buf) as usize;
            let mut frame = vec![0u8; frame_len];
            stream.read_exact(&mut frame).await.expect("frame body");
            let rep = marshal::decode(&frame).expect("decode macho frame");
            let pkt = MachoPacket::from_pyrep(&rep).expect("MachoPacket");
            if pkt.packet_type == MachoPacketType::CallRsp {
                return (frame, pkt);
            }
            assert_ne!(
                pkt.packet_type,
                MachoPacketType::ErrorResponse,
                "unexpected ErrorResponse on machoNet call: {rep:?}"
            );
        }
    }

    let assert_no_bare_class_bytes = |wire: &[u8], bare: &[u8], full: &[u8]| {
        let prefix = b"carbon.common.script.net.";
        assert!(
            wire.windows(full.len()).any(|w| w == full),
            "wire bytes must contain full V24 class path {}",
            String::from_utf8_lossy(full)
        );
        for (idx, w) in wire.windows(bare.len()).enumerate() {
            if w == bare {
                assert!(
                    idx >= prefix.len() && &wire[idx - prefix.len()..idx] == prefix,
                    "wire bytes contained unprefixed legacy class {} at offset {idx}",
                    String::from_utf8_lossy(bare)
                );
            }
        }
    };

    // 1. Initial marketProxy.GetOrders(34) with machoVersion=1 -> V24 CachedMethodCallResult
    write_framed_pyrep(
        &mut s,
        &send_call(
            2,
            "marketProxy",
            "GetOrders",
            vec![PyRep::Integer(34)],
            vec![(PyRep::String("machoVersion".into()), PyRep::Integer(1))],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    let (frame1, rsp1) = read_call_rsp_wire(&mut s).await;
    assert_no_bare_class_bytes(
        &frame1,
        b"objectCaching.CachedMethodCallResult",
        V24_CACHED_METHOD_CALL_RESULT_CLASS.as_bytes(),
    );
    let PyRep::Tuple(outer1) = &rsp1.payload else {
        panic!("CallRsp payload tuple");
    };
    let PyRep::SubStream(sub1) = &outer1[0] else {
        panic!("CallRsp SubStream");
    };
    let sub1_wire = marshal::encode(sub1);
    assert_eq!(sub1_wire[5], MarshalOpcode::PyObject as u8);
    assert_eq!(sub1_wire[6], MarshalOpcode::PyShortString as u8);
    assert_eq!(sub1_wire[7] as usize, V24_CACHED_METHOD_CALL_RESULT_CLASS.len());

    let PyRep::Object { type_name: t1, args: a1 } = sub1.as_ref() else {
        panic!("expected CachedMethodCallResult Object");
    };
    assert_eq!(t1, V24_CACHED_METHOD_CALL_RESULT_CLASS);
    let PyRep::Tuple(cmcr1) = a1.as_ref() else {
        panic!("expected 3-tuple");
    };
    assert_eq!(
        cmcr1[0].dict_get("versionCheck"),
        Some(&PyRep::Tuple(vec![
            PyRep::String("always".into()),
            PyRep::None,
            PyRep::None,
        ]))
    );
    let PyRep::List(ver1) = &cmcr1[2] else {
        panic!("expected 2-element version list");
    };
    let (ts1, crc1) = (ver1[0].as_i64().unwrap(), ver1[1].as_i64().unwrap());
    let PyRep::SubStream(orders1_rep) = &cmcr1[1] else {
        panic!("expected PySubStream result");
    };
    let PyRep::Tuple(orders1_pair) = orders1_rep.as_ref() else {
        panic!("expected (sell_crowset, buy_crowset)");
    };
    let PyRep::ObjectEx { list: sell_rows1, .. } = &orders1_pair[0] else {
        panic!("expected sell CRowset");
    };
    assert_eq!(sell_rows1.len(), 1);
    assert_eq!(sell_rows1[0].dbrow_get("volRemaining").and_then(|v| v.as_f64()), Some(1_962.0));
    assert_eq!(sell_rows1[0].dbrow_get("price").and_then(|v| v.as_f64()), Some(2.04));

    // 2. PlaceBuyOrder for 1 Tritanium @ 2.04 ISK, then re-fetch GetOrders(34) with machoVersion=[ts1, crc1]
    write_framed_pyrep(
        &mut s,
        &send_call(
            3,
            "marketProxy",
            "PlaceBuyOrder",
            vec![
                PyRep::Integer(60_003_760),
                PyRep::Integer(34),
                PyRep::Real(2.04),
                PyRep::Integer(1),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::None,
            ],
            vec![],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    let (_buy_frame, _buy_rsp) = read_call_rsp_wire(&mut s).await;

    write_framed_pyrep(
        &mut s,
        &send_call(
            4,
            "marketProxy",
            "GetOrders",
            vec![PyRep::Integer(34)],
            vec![(
                PyRep::String("machoVersion".into()),
                PyRep::List(vec![PyRep::Integer(ts1), PyRep::Integer(crc1)]),
            )],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    let (frame2, rsp2) = read_call_rsp_wire(&mut s).await;
    assert_no_bare_class_bytes(
        &frame2,
        b"objectCaching.CachedMethodCallResult",
        V24_CACHED_METHOD_CALL_RESULT_CLASS.as_bytes(),
    );
    let PyRep::Tuple(outer2) = &rsp2.payload else { panic!() };
    let PyRep::SubStream(sub2) = &outer2[0] else { panic!() };
    let PyRep::Object { type_name: t2, args: a2 } = sub2.as_ref() else { panic!() };
    assert_eq!(t2, V24_CACHED_METHOD_CALL_RESULT_CLASS);
    let PyRep::Tuple(cmcr2) = a2.as_ref() else { panic!() };
    let PyRep::List(ver2) = &cmcr2[2] else { panic!() };
    let (ts2, crc2) = (ver2[0].as_i64().unwrap(), ver2[1].as_i64().unwrap());
    assert!(ts2 > ts1, "version timestamp must advance: {ts2} > {ts1}");
    assert_ne!(crc2, crc1, "RefreshOrderCache requires CRC32 version[1] change after fill");
    let PyRep::SubStream(orders2_rep) = &cmcr2[1] else { panic!() };
    let PyRep::Tuple(orders2_pair) = orders2_rep.as_ref() else { panic!() };
    let PyRep::ObjectEx { list: sell_rows2, .. } = &orders2_pair[0] else { panic!() };
    assert_eq!(sell_rows2[0].dbrow_get("volRemaining").and_then(|v| v.as_f64()), Some(1_961.0));

    // 3. marketProxy.GetPlexOrders() also wraps in V24 CachedMethodCallResult
    write_framed_pyrep(
        &mut s,
        &send_call(5, "marketProxy", "GetPlexOrders", vec![], vec![]).to_pyrep(),
    )
    .await
    .unwrap();
    let (plex_frame, plex_rsp) = read_call_rsp_wire(&mut s).await;
    assert_no_bare_class_bytes(
        &plex_frame,
        b"objectCaching.CachedMethodCallResult",
        V24_CACHED_METHOD_CALL_RESULT_CLASS.as_bytes(),
    );
    let PyRep::Tuple(plex_outer) = &plex_rsp.payload else { panic!() };
    let PyRep::SubStream(plex_sub) = &plex_outer[0] else { panic!() };
    let PyRep::Object { type_name: pt, args: pa } = plex_sub.as_ref() else { panic!() };
    assert_eq!(pt, V24_CACHED_METHOD_CALL_RESULT_CLASS);
    let PyRep::Tuple(pcmcr) = pa.as_ref() else { panic!() };
    let PyRep::SubStream(plex_orders_rep) = &pcmcr[1] else { panic!() };
    let PyRep::Tuple(plex_pair) = plex_orders_rep.as_ref() else { panic!() };
    let PyRep::ObjectEx { list: plex_sells, .. } = &plex_pair[0] else { panic!() };
    assert_eq!(plex_sells.len(), 1);
    assert_eq!(plex_sells[0].dbrow_get("volRemaining").and_then(|v| v.as_f64()), Some(10.0));

    // 4. objectCaching.GetCachableObject & machoNet.GetCachedObject emit V24 CachedObject (0x17)
    for (call_id, svc, method, args) in [
        (
            6,
            "objectCaching",
            "GetCachableObject",
            vec![
                PyRep::Bool(false),
                PyRep::String("config.Units".into()),
                PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(0)]),
                PyRep::Integer(888_444),
            ],
        ),
        (
            7,
            "machoNet",
            "GetCachedObject",
            vec![PyRep::String("config.Units".into())],
        ),
    ] {
        write_framed_pyrep(&mut s, &send_call(call_id, svc, method, args, vec![]).to_pyrep())
            .await
            .unwrap();
        let (co_frame, co_rsp) = read_call_rsp_wire(&mut s).await;
        assert_no_bare_class_bytes(
            &co_frame,
            b"objectCaching.CachedObject",
            V24_CACHED_OBJECT_CLASS.as_bytes(),
        );
        let PyRep::Tuple(co_outer) = &co_rsp.payload else { panic!() };
        let PyRep::SubStream(co_sub) = &co_outer[0] else { panic!() };
        let co_wire = marshal::encode(co_sub);
        assert_eq!(co_wire[5], MarshalOpcode::PyObject as u8);
        let PyRep::Object { type_name, args } = co_sub.as_ref() else {
            panic!("expected CachedObject");
        };
        assert_eq!(type_name, V24_CACHED_OBJECT_CLASS);
        let PyRep::Tuple(fields) = args.as_ref() else { panic!("7-tuple") };
        assert_eq!(fields.len(), 7);
        let PyRep::Buffer(inner_bytes) = &fields[4] else { panic!("Buffer") };
        let inner = marshal::decode(inner_bytes).expect("decode CachedObject pickle");
        assert!(matches!(inner, PyRep::IndexRowset { .. }));
    }
}
