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
