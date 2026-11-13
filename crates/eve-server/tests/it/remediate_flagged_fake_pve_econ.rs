//! End-to-end integration tests for Project Discovery, Fast Checkout, New Eden Store,
//! Daily Login Campaigns, Agent Missions, AIR Career Program, Daily Goals, and Q-6 Quasar Economy/Industry.
//! Exercises real MachoNet / Quasar RPC dispatch, HTTP/2 gRPC transport, notifications, error paths,
//! side effects, and SQLite reload persistence without snapshot mutation bypasses.

#![forbid(unsafe_code)]

use crate::{common, pve_e2e_common::*};
use bytes::{BufMut, Bytes, BytesMut};
use eve_db::InventoryFlag;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, EvePublicNotice, EvePublicRequest, EvePublicResponse,
    GoogleAny, GoogleTimestamp, QuasarMessage, WireMessage, WireValue, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK};
use eve_server::router::ServerContext;

const ERR_PD_TOO_FAST: &str = "ProjectDiscoverySubmissionTooFast";
const ERR_PD_ALREADY_SUBMITTED: &str = "TaskAlreadySubmitted";
const ERR_DAILY_ALREADY_CLAIMED: &str = "DailyRewardAlreadyClaimedToday";
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc::Receiver;

fn fresh_ctx_full_market() -> ServerContext {
    static BASE: OnceLock<(Arc<eve_sde::SdeRegistry>, eve_db::WorldStateSnapshot)> = OnceLock::new();
    let (sde, snap) = BASE.get_or_init(|| {
        let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
        let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
        let _ = eve_server::world_seeder::seed_dual_anchor_markets(&db, &sde);
        let _ = eve_server::world_seeder::seed_full_market_catalog(&db, &sde);
        (sde, db.with_snapshot(|s| s.clone()))
    });
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    db.with_mut_snapshot(|s| *s = snap.clone());
    let ctx = ServerContext::with_sde_and_repository(Arc::clone(sde), db)
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED);
    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .free_fast_checkout_plex = true;
    crate::common::pin_clock(&ctx);
    ctx
}

async fn login_demo_gm(ctx: &ServerContext, sess_id: u64) -> SessionState {
    if !ctx.db.with_snapshot(|s| s.accounts.contains_key("gm_remediate")) {
        ctx.db
            .provision_gm_account("gm_remediate", "gm-pass", 90_000_099, "GM Remediate");
    }
    let mut gm = SessionState::new(sess_id);
    gm.address = "127.0.0.1:26000".into();
    common::login_pilot(ctx, &mut gm, "gm_remediate", "gm-pass").await;
    rpc_ok(
        ctx,
        &mut gm,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_099)],
    )
    .await;
    gm.character_id = Some(90_000_099);
    gm.corporation_id = ctx
        .db
        .get_character(90_000_099)
        .ok()
        .map(|c| c.corporation_id)
        .or(Some(98_000_001));
    gm.solar_system_id = Some(JITA_SYS);
    gm.station_id = Some(JITA_STATION);
    assert_ne!(gm.role & service_roles::ROLE_GM_MASK, 0);
    gm
}

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b)
                        .expect("decode EvePublicResponse from response_proto");
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
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: format!("pve-econ-req-{n:06}").into_bytes(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"pve-econ-app-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = rpc_ok(
        ctx,
        sess,
        "publicGatewaySvc",
        "SendRequest",
        &[PyRep::Bytes(req.encode_proto())],
    )
    .await;
    extract_response_proto(&rep)
}

async fn subscribe_quasar(ctx: &ServerContext, sess: &mut SessionState) -> Receiver<EvePublicNotice> {
    ctx.quasar_gateway
        .subscribe_session(sess, Vec::new(), sess.session_id)
}

fn drain_quasar_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<GoogleAny> {
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        if let Some(p) = n.payload {
            out.push(p);
        }
    }
    out
}

fn id_payload(tag: u32, id: i64) -> Vec<u8> {
    let mut inner = Vec::new();
    encode_varint_field(1, id as u64, &mut inner);
    let mut out = Vec::new();
    encode_bytes_field(tag, &inner, &mut out);
    out
}

fn uuid_req(uuid: [u8; 16]) -> Vec<u8> {
    let mut inner = Vec::new();
    encode_bytes_field(1, &uuid, &mut inner);
    let mut out = Vec::new();
    encode_bytes_field(1, &inner, &mut out);
    out
}

fn extract_16byte_uuids(buf: &[u8]) -> Vec<[u8; 16]> {
    let mut out = Vec::new();
    let Ok(msg) = WireMessage::decode(buf) else {
        return out;
    };
    for f in msg.fields {
        if let WireValue::LengthDelimited(bytes) = f.value {
            if bytes.len() == 16 {
                let mut u = [0u8; 16];
                u.copy_from_slice(&bytes);
                if !out.contains(&u) {
                    out.push(u);
                }
            } else {
                for nested in extract_16byte_uuids(&bytes) {
                    if !out.contains(&nested) {
                        out.push(nested);
                    }
                }
            }
        }
    }
    out
}

fn find_tuple_i64(rep: &PyRep, key: &str) -> Option<i64> {
    kv_get(rep, key).and_then(PyRep::as_i64)
}

fn find_tuple_f64(rep: &PyRep, key: &str) -> Option<f64> {
    kv_get(rep, key).and_then(PyRep::as_f64)
}

fn count_hangar_items_of_type(
    ctx: &ServerContext,
    char_id: i64,
    location_id: i64,
    type_id: u32,
) -> i64 {
    ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|it| {
                it.owner_id == char_id
                    && it.location_id == location_id
                    && it.type_id == type_id
            })
            .map(|it| i64::from(it.quantity))
            .sum()
    })
}

async fn fly_stargates_to_fast(ctx: &ServerContext, sess: &mut SessionState, dest_sys: u32) {
    let start_sys = sess.solar_system_id.unwrap_or(JITA_SYS);
    if start_sys == dest_sys {
        return;
    }
    if sess.station_id.is_some() {
        undock_from_station(ctx, sess, start_sys).await;
        ctx.advance_secs(11);
    }
    let ship_id = sess.ship_id.expect("active ship in fly_stargates_to_fast");
    let (mut visited, mut prev, mut queue) = (
        HashSet::from([start_sys]),
        HashMap::new(),
        VecDeque::from([start_sys]),
    );
    while let Some(cur) = queue.pop_front() {
        if cur == dest_sys {
            break;
        }
        for j in ctx.sde.jumps.values().filter(|j| {
            j.from_solar_system_id == cur && (j.to_solar_system_id != 30_100_000 || dest_sys == 30_100_000)
        }) {
            if visited.insert(j.to_solar_system_id) {
                prev.insert(j.to_solar_system_id, (cur, j.stargate_id, j.destination_stargate_id));
                queue.push_back(j.to_solar_system_id);
            }
        }
    }
    let (mut hops, mut trace) = (Vec::new(), dest_sys);
    while trace != start_sys {
        let (p_sys, fg, tg) = prev[&trace];
        hops.push((p_sys, fg, tg, trace));
        trace = p_sys;
    }
    hops.reverse();
    for (cur_sys, fg, _tg, next_sys) in hops {
        let _ = bind_service(ctx, sess, "beyonce", PyRep::Integer(i64::from(cur_sys))).await;
        if let Some(gate) = ctx.sde.stargates().get(&fg) {
            let sys_h = ctx.systems.get_or_spawn(cur_sys);
            let _ = sys_h.set_ball_position(ship_id, gate.position).await;
        }
        ctx.advance_secs(11);
        rpc_ok(
            ctx,
            sess,
            "beyonce",
            "CmdStargateJump",
            &[PyRep::Integer(fg), PyRep::Integer(i64::from(next_sys))],
        )
        .await;
        sess.solar_system_id = Some(next_sys);
    }
    sess.solar_system_id = Some(dest_sys);
    sess.station_id = None;
    sess.region_id = ctx.sde.solar_systems.get(&dest_sys).map(|s| s.region_id);
    let _ = bind_service(ctx, sess, "beyonce", PyRep::Integer(i64::from(dest_sys))).await;
}

async fn warp_and_dock_at_station_fast(
    ctx: &ServerContext,
    sess: &mut SessionState,
    ship_id: i64,
    sys_id: u32,
    target_station_id: i64,
) {
    if sess.station_id == Some(target_station_id) {
        return;
    }
    if sess.station_id.is_some() {
        undock_from_station(ctx, sess, sess.solar_system_id.unwrap_or(sys_id)).await;
    }
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    if let Some(st) = ctx.sde.stations.get(&target_station_id) {
        let _ = sys_h.set_ball_position(ship_id, st.position).await;
    }
    ctx.advance_secs(11);
    let b_oid = bind_service(ctx, sess, "beyonce", PyRep::Integer(i64::from(sys_id))).await as u64;
    ctx.dispatch_rpc(
        sess,
        "beyonce",
        Some(b_oid),
        "CmdDock",
        &[PyRep::Integer(target_station_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("CmdDock");
    for _ in 0..10 {
        if sess
            .character_id
            .is_some_and(|cid| ctx.db.get_character(cid).ok().and_then(|c| c.station_id) == Some(target_station_id))
        {
            break;
        }
        let _ = sys_h.step_manual(1.0).await;
        ctx.advance_secs(10);
    }
    if let Some(ch) = sess.character_id.and_then(|cid| ctx.db.get_character(cid).ok()) {
        sess.solar_system_id = Some(ch.solar_system_id);
        sess.station_id = ch.station_id;
        sess.region_id = ctx.sde.solar_systems.get(&ch.solar_system_id).map(|s| s.region_id);
    }
}

async fn create_player_pilot_fast(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    name: &str,
    solar_system_id: u32,
) -> (SessionState, i64, i64) {
    let pass = format!("{user}-secret");
    if !ctx.db.with_snapshot(|s| s.accounts.contains_key(user)) {
        ctx.db.provision_player_account(user, &pass);
    }
    let mut sess = SessionState::new(sess_id);
    sess.address = "127.0.0.1:26000".into();
    common::login_pilot(ctx, &mut sess, user, &pass).await;
    let doll = PyRep::keyval([
        ("costume", PyRep::Integer(301)),
        ("gender", PyRep::Integer(1)),
        ("hair", PyRep::Integer(101)),
        ("skin", PyRep::Integer(201)),
    ]);
    let portrait = PyRep::keyval([
        ("background", PyRep::Integer(42)),
        ("camera", PyRep::Integer(1)),
        ("pose", PyRep::Integer(7)),
    ]);
    let anc = ctx
        .sde
        .ancestries
        .values()
        .find(|a| a.bloodline_id == 1)
        .map(|a| i64::from(a.ancestry_id))
        .unwrap_or(1);
    let sch = ctx
        .sde
        .schools
        .values()
        .filter(|s| s.race_id == 1)
        .min_by_key(|s| {
            let mut vis = HashSet::from([s.solar_system_id]);
            let mut q = VecDeque::from([(s.solar_system_id, 0u32)]);
            while let Some((cur, d)) = q.pop_front() {
                if cur == JITA_SYS {
                    return d;
                }
                for j in ctx.sde.jumps.values().filter(|j| j.from_solar_system_id == cur) {
                    if vis.insert(j.to_solar_system_id) {
                        q.push_back((j.to_solar_system_id, d + 1));
                    }
                }
            }
            u32::MAX
        })
        .map(|s| i64::from(s.school_id))
        .unwrap_or(11);
    let char_id = rpc_ok(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[
            PyRep::String(name.into()),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(anc),
            doll,
            portrait,
            PyRep::Integer(sch),
            PyRep::None,
            PyRep::Integer(i64::from(solar_system_id.max(JITA_SYS))),
        ],
    )
    .await
    .as_i64()
    .expect("CreateCharacterWithDoll returned char_id");
    rpc_ok(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await;
    let ch = ctx.db.get_character(char_id).expect("character exists");
    sess.character_id = Some(ch.character_id);
    sess.corporation_id = Some(ch.corporation_id);
    sess.solar_system_id = Some(ch.solar_system_id);
    sess.station_id = ch.station_id;
    sess.region_id = ctx
        .sde
        .solar_systems
        .get(&ch.solar_system_id)
        .map(|s| s.region_id);
    let ship_id = ch.active_ship_id;
    assert!(ship_id > 0, "active ship id");
    sess.ship_id = Some(ship_id);
    fund_pilot_isk(ctx, &mut sess, 3).await;
    if solar_system_id > 0 {
        if let Some(cur_sys) = sess.solar_system_id.filter(|&s| s != JITA_SYS) {
            if sess.station_id.is_some() {
                undock_from_station(ctx, &mut sess, cur_sys).await;
            }
            fly_stargates_to_fast(ctx, &mut sess, JITA_SYS).await;
        }
        if sess.station_id != Some(JITA_STATION) {
            warp_and_dock_at_station_fast(ctx, &mut sess, ship_id, JITA_SYS, JITA_STATION).await;
        }
    }
    (sess, char_id, ship_id)
}

async fn setup_pilot_in_full_market(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    char_name: &str,
) -> (SessionState, i64, i64) {
    let pass = format!("{user}-secret");
    if !ctx.db.with_snapshot(|s| s.accounts.contains_key(user)) {
        ctx.db.provision_player_account(user, &pass);
    }
    let mut sess = SessionState::new(sess_id);
    sess.address = "127.0.0.1:26000".into();
    common::login_pilot(ctx, &mut sess, user, &pass).await;
    let doll = PyRep::keyval([
        ("costume", PyRep::Integer(301)),
        ("gender", PyRep::Integer(1)),
        ("hair", PyRep::Integer(101)),
        ("skin", PyRep::Integer(201)),
    ]);
    let portrait = PyRep::keyval([
        ("background", PyRep::Integer(42)),
        ("camera", PyRep::Integer(1)),
        ("pose", PyRep::Integer(7)),
    ]);
    let anc = ctx
        .sde
        .ancestries
        .values()
        .find(|a| a.bloodline_id == 1)
        .map(|a| i64::from(a.ancestry_id))
        .unwrap_or(11);
    let sch = ctx
        .sde
        .schools
        .values()
        .filter(|s| s.race_id == 1)
        .map(|s| i64::from(s.school_id))
        .min()
        .unwrap_or(11);
    let char_id = rpc_ok(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[
            PyRep::String(char_name.into()),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(anc),
            doll,
            portrait,
            PyRep::Integer(sch),
            PyRep::None,
            PyRep::Integer(i64::from(JITA_SYS)),
        ],
    )
    .await
    .as_i64()
    .expect("created char_id");
    rpc_ok(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await;
    let ch = ctx.db.get_character(char_id).expect("character exists");
    sess.character_id = Some(ch.character_id);
    sess.corporation_id = Some(ch.corporation_id);
    sess.solar_system_id = Some(ch.solar_system_id);
    sess.station_id = ch.station_id;
    sess.region_id = ctx
        .sde
        .solar_systems
        .get(&ch.solar_system_id)
        .map(|s| s.region_id);
    sess.ship_id = Some(ch.active_ship_id);
    if sess.solar_system_id != Some(JITA_SYS) {
        if sess.station_id.is_some() {
            let cur_sys = sess.solar_system_id.unwrap();
            undock_from_station(ctx, &mut sess, cur_sys).await;
        }
        fly_stargates_to_fast(ctx, &mut sess, JITA_SYS).await;
    }
    if sess.station_id != Some(JITA_STATION) {
        warp_and_dock_at_station_fast(ctx, &mut sess, ch.active_ship_id, JITA_SYS, JITA_STATION).await;
    }
    // Fund ISK legitimately through PLEX sell order when free_fast_checkout_plex is enabled
    if ctx.feature_flags_snapshot().free_fast_checkout_plex {
        fund_pilot_isk(ctx, &mut sess, 20).await;
    }
    (sess, char_id, ch.active_ship_id)
}

async fn buy_item_valid_tick(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    station_id: i64,
    type_id: u32,
    qty: i64,
) -> i64 {
    let before_ids: std::collections::HashSet<i64> = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|it| it.owner_id == char_id && it.type_id == type_id)
            .map(|it| it.item_id)
            .collect()
    });
    let st_id = sess.station_id.unwrap_or(station_id);
    let min_sell = ctx
        .db
        .with_snapshot(|s| {
            s.market_orders
                .values()
                .filter(|o| {
                    !o.is_bid && o.station_id == st_id && o.type_id == type_id && o.vol_remaining > 0
                })
                .map(|o| o.price.to_f64_isk())
                .reduce(f64::min)
        })
        .unwrap_or_else(|| {
            ctx.sde
                .types
                .get(&type_id)
                .map_or(5_000.0, |t| t.base_price.max(100.0) * 1.25)
        });
    let raw = (min_sell * 1.15).ceil().max(1.0);
    let exp = (raw.log10().floor() as i32 - 2).max(0);
    let step = 10.0_f64.powi(exp);
    let price = ((raw / step).ceil() * step).max(1.0);
    rpc_ok(
        ctx,
        sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(st_id),
            PyRep::Integer(i64::from(type_id)),
            PyRep::Real(price),
            PyRep::Integer(qty),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
    )
    .await;
    ctx.db
        .with_snapshot(|snap| {
            snap.items
                .values()
                .find(|it| {
                    it.owner_id == char_id
                        && it.type_id == type_id
                        && !before_ids.contains(&it.item_id)
                })
                .or_else(|| {
                    snap.items
                        .values()
                        .find(|it| it.owner_id == char_id && it.type_id == type_id)
                })
                .map(|it| it.item_id)
        })
        .unwrap_or_else(|| panic!("Purchased item type {type_id} not found for {char_id}"))
}

async fn gm_supply_and_buy(
    ctx: &ServerContext,
    gm: &mut SessionState,
    sess: &mut SessionState,
    char_id: i64,
    station_id: i64,
    type_id: u32,
    qty: i64,
) -> i64 {
    let has_stock = ctx.db.with_snapshot(|s| {
        s.market_orders
            .values()
            .filter(|o| {
                !o.is_bid && o.station_id == station_id && o.type_id == type_id && o.vol_remaining > 0
            })
            .map(|o| i64::from(o.vol_remaining))
            .sum::<i64>()
            >= qty
    });
    if !has_stock {
        let gm_item_id = common::slash_cmd(ctx, gm, &format!("/createitem {type_id} {qty}"))
            .await
            .dict_get("itemID")
            .and_then(PyRep::as_i64)
            .expect("gm createitem itemID");
        rpc_ok(
            ctx,
            gm,
            "marketProxy",
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::Dict(vec![
                    (PyRep::String("itemID".into()), PyRep::Integer(gm_item_id)),
                    (PyRep::String("typeID".into()), PyRep::Integer(i64::from(type_id))),
                    (PyRep::String("price".into()), PyRep::Real(100.0)),
                    (PyRep::String("quantity".into()), PyRep::Integer(qty)),
                    (PyRep::String("stationID".into()), PyRep::Integer(station_id)),
                    (PyRep::String("duration".into()), PyRep::Integer(90)),
                ])]),
                PyRep::Bool(false),
            ],
        )
        .await;
    }
    buy_item_valid_tick(ctx, sess, char_id, station_id, type_id, qty).await
}

fn pick_small_seeded_bpo(ctx: &ServerContext, station_id: i64) -> (u32, Vec<eve_sde::BlueprintMaterial>) {
    ctx.db.with_snapshot(|s| {
        let mut max_ask_by_type: HashMap<u32, i64> = HashMap::new();
        for o in s.market_orders.values() {
            if !o.is_bid && o.station_id == station_id {
                let qty = i64::from(o.vol_remaining);
                max_ask_by_type
                    .entry(o.type_id)
                    .and_modify(|e| {
                        if qty > *e {
                            *e = qty;
                        }
                    })
                    .or_insert(qty);
            }
        }
        let bp = ctx
            .sde
            .blueprints
            .values()
            .filter(|bp| {
                ctx.sde
                    .types
                    .get(&bp.blueprint_type_id)
                    .is_some_and(|t| t.published && t.category_id == 9)
                    && !bp.materials.is_empty()
                    && max_ask_by_type.get(&bp.blueprint_type_id).copied().unwrap_or(0) >= 1
                    && bp.materials.iter().all(|m| {
                        m.quantity > 0
                            && m.quantity <= 500
                            && max_ask_by_type.get(&m.type_id).copied().unwrap_or(0) >= m.quantity as i64
                    })
            })
            .min_by_key(|bp| bp.blueprint_type_id)
            .expect("small seeded BPO in SDE + Jita market");
        (bp.blueprint_type_id, bp.materials.clone())
    })
}

async fn send_h2c_quasar_request(
    addr: std::net::SocketAddr,
    bearer_token: Option<&str>,
    req: &EvePublicRequest,
) -> (Option<String>, Option<EvePublicResponse>) {
    let tcp = {
        let mut last_err = None;
        let mut stream = None;
        for attempt in 0..5_usize {
            tokio::task::yield_now().await;
            match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                tokio::net::TcpStream::connect(addr),
            )
            .await
            {
                Ok(Ok(s)) => {
                    stream = Some(s);
                    break;
                }
                Ok(Err(e)) => {
                    last_err = Some(e.to_string());
                }
                Err(_) => {
                    last_err = Some("connect timed out after 5s".to_string());
                }
            }
            if attempt + 1 < 5 {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        }
        stream.unwrap_or_else(|| {
            panic!(
                "connect to h2c Quasar gateway {addr}: {}",
                last_err.unwrap_or_else(|| "unknown error".into())
            )
        })
    };
    let (client, conn) = h2::client::handshake(tcp).await.expect("h2 handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut http_req = http::Request::builder()
        .method("POST")
        .uri(format!("http://{addr}/eve_public.gateway.Requests/Send"))
        .header("content-type", "application/grpc")
        .header("te", "trailers");
    if let Some(tok) = bearer_token {
        http_req = http_req.header("authorization", format!("Bearer {tok}"));
    }
    let http_req = http_req.body(()).expect("build h2 request");
    let body = req.encode_proto();
    let mut frame = BytesMut::with_capacity(5 + body.len());
    frame.put_u8(0);
    frame.put_u32(body.len() as u32);
    frame.extend_from_slice(&body);

    let mut client = client.ready().await.expect("h2 client ready");
    let (resp_fut, mut send_stream) = client.send_request(http_req, false).expect("send h2 headers");
    // An h2c server rejecting at header auth (`send_trailers_only`) drops `RecvStream` and sends
    // `RST_STREAM(NO_ERROR)` immediately, which can race ahead of `send_data` under CPU load and
    // surface as `User(InactiveStreamId)`. Preserve any send error unless header status arrived.
    let send_err = send_stream.send_data(frame.freeze(), true).err();

    let resp = resp_fut
        .await
        .unwrap_or_else(|e| panic!("await h2 response headers (send_err={send_err:?}): {e}"));
    let header_status = resp
        .headers()
        .get("grpc-status")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if let Some(err) = send_err {
        if header_status.is_none() {
            panic!("send gRPC frame: {err}");
        }
    }
    let mut recv = resp.into_body();
    let mut data_buf = BytesMut::new();
    while let Some(chunk) = recv.data().await {
        let bytes: Bytes = chunk.expect("read h2 body chunk");
        let _ = recv.flow_control().release_capacity(bytes.len());
        data_buf.extend_from_slice(&bytes);
    }
    let trailer_status = if header_status.is_some() && recv.is_end_stream() {
        None
    } else {
        recv.trailers()
            .await
            .expect("read h2 trailers")
            .and_then(|t| {
                t.get("grpc-status")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            })
    };
    let grpc_status = trailer_status.or(header_status);

    let decoded = if data_buf.len() >= 5 {
        let msg_len = u32::from_be_bytes([data_buf[1], data_buf[2], data_buf[3], data_buf[4]]) as usize;
        let payload = &data_buf[5..5 + msg_len];
        Some(EvePublicResponse::decode_proto(payload).expect("decode EvePublicResponse from h2"))
    } else {
        None
    };
    (grpc_status, decoded)
}

#[tokio::test]
async fn project_discovery_tutorial_and_state_lifecycle() {
    // parity: rpc:ProjectDiscovery.get_player_state, rpc:ProjectDiscovery.get_player_analysis_kredits, rpc:ProjectDiscovery.get_player_statistics, rpc:ProjectDiscovery.get_total_needed_xp, rpc:ProjectDiscovery.get_tutorial_xp_reward, rpc:ProjectDiscovery.give_tutorial_rewards, rpc:ProjectDiscovery.get_maximum_bonus_samples, rpc:ProjectDiscovery.get_remaining_bonus_samples, rpc:ProjectDiscovery.new_training_task
    eve_testkit::attest(&[
        "rpc:ProjectDiscovery.get_player_state",
        "rpc:ProjectDiscovery.get_player_analysis_kredits",
        "rpc:ProjectDiscovery.get_player_statistics",
        "rpc:ProjectDiscovery.get_total_needed_xp",
        "rpc:ProjectDiscovery.get_tutorial_xp_reward",
        "rpc:ProjectDiscovery.give_tutorial_rewards",
        "rpc:ProjectDiscovery.get_maximum_bonus_samples",
        "rpc:ProjectDiscovery.get_remaining_bonus_samples",
        "rpc:ProjectDiscovery.new_training_task",
    ]);

    let ctx = fresh_player_ctx();
    // Unauthenticated calls must fail cleanly
    let mut unauth = SessionState::new(60_001);
    let err_unauth = rpc_err(&ctx, &mut unauth, "ProjectDiscovery", "get_player_state", &[]).await;
    assert!(!err_unauth.code.is_empty());

    let (mut sess, char_id, _) =
        create_player_pilot_fast(&ctx, 60_002, "pd_tut_user", "PD Tutorial Pilot", JITA_SYS).await;
    assert_eq!(sess.role & service_roles::ROLE_GM_MASK, 0);

    // 1. Initial tutorial status & error before completion
    let init = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "initialize_tutorial_status",
        &[],
    )
    .await;
    assert_eq!(init, PyRep::Bool(false));

    let early_claim = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "give_tutorial_rewards",
        &[],
    )
    .await;
    assert_eq!(early_claim, PyRep::Bool(false));

    let xp_reward_rep = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_tutorial_xp_reward",
        &[],
    )
    .await;
    let tut_xp = xp_reward_rep.as_f64().expect("tutorial xp float");
    assert!(tut_xp > 0.0, "tutorial XP reward must be positive");

    // 2. Step through tutorial training tasks & levels 1..=5
    let train_task = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "new_training_task",
        &[PyRep::Integer(1)],
    )
    .await;
    assert!(
        kv_get(&train_task, "taskID").is_some()
            || train_task.dict_get("task_id").is_some()
            || train_task.dict_get("taskId").is_some()
            || train_task.dict_get("id").is_some()
    );
    for _ in 0..5 {
        let _ = rpc_ok(
            &ctx,
            &mut sess,
            "ProjectDiscovery",
            "increase_tutorial_level",
            &[],
        )
        .await;
    }
    let level_rep = rpc_ok(&ctx, &mut sess, "ProjectDiscovery", "get_tutorial_level", &[]).await;
    assert!(level_rep.as_i64().unwrap_or(0) >= 5);

    // 3. Claim tutorial rewards -> grants tutorial XP (`Bool(true)`); repeat claim returns `Bool(false)`
    let reward_rep = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "give_tutorial_rewards",
        &[],
    )
    .await;
    assert_eq!(reward_rep, PyRep::Bool(true));

    let dup_claim = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "give_tutorial_rewards",
        &[],
    )
    .await;
    assert_eq!(dup_claim, PyRep::Bool(false));

    // 4. Player state, statistics, XP threshold progression, and Alpha bonus samples (10)
    let state = rpc_ok(&ctx, &mut sess, "ProjectDiscovery", "get_player_state", &[]).await;
    let exp_before = find_tuple_f64(&state, "experience")
        .or_else(|| state.dict_get("experience").and_then(PyRep::as_f64))
        .unwrap_or(0.0);
    assert!(exp_before >= tut_xp);

    let stats = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_player_statistics",
        &[],
    )
    .await;
    let needed_r0 = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_total_needed_xp",
        &[PyRep::Integer(0)],
    )
    .await
    .as_f64()
    .expect("needed xp r0");
    let needed_r1 = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_total_needed_xp",
        &[PyRep::Integer(1)],
    )
    .await
    .as_f64()
    .expect("needed xp r1");
    assert!(needed_r1 > needed_r0 && needed_r1 > 0.0);
    assert!(stats.dict_get("experience").is_some() || kv_get(&stats, "projects").is_some());

    let max_bonus = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_maximum_bonus_samples",
        &[],
    )
    .await
    .as_i64()
    .unwrap();
    let rem_bonus = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_remaining_bonus_samples",
        &[],
    )
    .await
    .as_i64()
    .unwrap();
    assert_eq!(max_bonus, 10);
    assert_eq!(rem_bonus, 10);

    // 5. SQLite reload preserves AK, XP, and bonus samples
    let ctx2 = reload_ctx_from_sqlite(&ctx);
    let (mut sess2, _) =
        login_existing_player(&ctx2, 60_003, "pd_tut_user", "pd_tut_user-secret", char_id).await;
    assert_eq!(
        rpc_ok(
            &ctx2,
            &mut sess2,
            "ProjectDiscovery",
            "get_player_analysis_kredits",
            &[],
        )
        .await
        .as_i64(),
        Some(0)
    );
    let state2 = rpc_ok(&ctx2, &mut sess2, "ProjectDiscovery", "get_player_state", &[]).await;
    assert!(
        (find_tuple_f64(&state2, "experience")
            .or_else(|| state2.dict_get("experience").and_then(PyRep::as_f64))
            .unwrap_or(0.0)
            - exp_before)
            .abs()
            < 1e-6
    );
    assert_eq!(
        rpc_ok(
            &ctx2,
            &mut sess2,
            "ProjectDiscovery",
            "get_remaining_bonus_samples",
            &[],
        )
        .await
        .as_i64(),
        Some(10)
    );
}

#[tokio::test]
async fn project_discovery_classification_rewards_notify_and_reload() {
    // parity: rpc:ProjectDiscovery.get_new_task, rpc:ProjectDiscovery.get_data_from_url, rpc:ProjectDiscovery.post_classification, rpc:ProjectDiscovery.get_remaining_bonus_samples, rpc:ProjectDiscovery.get_player_analysis_kredits, notify:OnAnalysisKreditsChange
    eve_testkit::attest(&[
        "rpc:ProjectDiscovery.get_new_task",
        "rpc:ProjectDiscovery.get_data_from_url",
        "rpc:ProjectDiscovery.post_classification",
        "rpc:ProjectDiscovery.get_remaining_bonus_samples",
        "rpc:ProjectDiscovery.get_player_analysis_kredits",
        "notify:OnAnalysisKreditsChange",
    ]);

    let ctx = fresh_player_ctx();
    let (mut sess, char_id, _) =
        create_player_pilot_fast(&ctx, 60_011, "pd_cls_user", "PD Classifier", JITA_SYS).await;
    let mut gm = login_demo_gm(&ctx, 60_012).await;

    // Configure PD parameters via GM slash command:
    // min_time=100ms, tut_xp=250, base_isk=150000, max_bonus=10, ak=15, cooldown=10s
    common::slash_cmd(&ctx, &mut gm, "/pd config 100 250 150000 10 15 10").await;
    common::slash_cmd(&ctx, &mut gm, "/pd consensus 1 true").await;

    let isk_before = ctx.db.get_character(char_id).unwrap().balance;
    let ak_before = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_player_analysis_kredits",
        &[],
    )
    .await
    .as_i64()
    .unwrap();
    let _ = drain_all_notifications(&ctx, &mut sess, char_id);

    // 1. Fetch new task & payload via get_data_from_url (and verify bogus URL rejected)
    let task_rep = rpc_ok(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[]).await;
    let task_id = kv_get(&task_rep, "taskID")
        .or_else(|| kv_get(&task_rep, "task_id"))
        .and_then(|v| v.as_str())
        .expect("taskID")
        .to_string();
    let data_url = kv_get(&task_rep, "url")
        .or_else(|| kv_get(&task_rep, "dataUrl"))
        .or_else(|| kv_get(&task_rep, "data_url"))
        .and_then(|v| v.as_str())
        .expect("dataUrl")
        .to_string();
    let payload_str = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_data_from_url",
        &[PyRep::String(data_url.clone())],
    )
    .await
    .as_str()
    .expect("task JSON payload")
    .to_string();
    let payload_json: serde_json::Value =
        serde_json::from_str(&payload_str).expect("valid task JSON");
    assert_eq!(payload_json["task_id"].as_str(), Some(task_id.as_str()));

    let bad_url = rpc_err(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_data_from_url",
        &[PyRep::String("https://invalid.example/no-such-task".into())],
    )
    .await;
    assert!(!bad_url.code.is_empty());

    // 2. Empty classification payload must fail without crediting AK or consuming bonus sample
    let empty_cls = rpc_err(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::String(task_id.clone()), PyRep::List(vec![])],
    )
    .await;
    assert!(!empty_cls.code.is_empty());
    assert_eq!(
        rpc_ok(
            &ctx,
            &mut sess,
            "ProjectDiscovery",
            "get_remaining_bonus_samples",
            &[],
        )
        .await
        .as_i64(),
        Some(10)
    );

    // 3. Valid accurate classification -> grants AK, XP, ISK, decrements bonus samples, emits OnAnalysisKreditsChange
    let cls_args = [PyRep::String(task_id.clone()), PyRep::List(vec![PyRep::Real(1.0)])];
    let cls_res = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &cls_args,
    )
    .await;
    let ak_delta = cls_res
        .dict_get("kredits_reward")
        .and_then(PyRep::as_i64)
        .or_else(|| find_tuple_i64(&cls_res, "AK_Reward"))
        .unwrap_or(0);
    assert!(ak_delta > 0, "accurate classification must pay positive kredits_reward: {cls_res:?}");

    let ak_after = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_player_analysis_kredits",
        &[],
    )
    .await
    .as_i64()
    .unwrap();
    assert_eq!(ak_after, ak_before + ak_delta);
    assert!(
        ctx.db.get_character(char_id).unwrap().balance > isk_before,
        "Project Discovery classification must credit ISK"
    );
    assert_eq!(
        rpc_ok(
            &ctx,
            &mut sess,
            "ProjectDiscovery",
            "get_remaining_bonus_samples",
            &[],
        )
        .await
        .as_i64(),
        Some(9)
    );

    let notifs = drain_all_notifications(&ctx, &mut sess, char_id);
    assert!(
        notifs.iter().any(|n| n.0 == "OnAnalysisKreditsChange"),
        "OnAnalysisKreditsChange must fire after AK increase, got {notifs:?}"
    );

    // 4. Rapid resubmit (< 10s cooldown) fails with ProjectDiscoverySubmissionTooFast;
    //    resubmit same task after cooldown fails with TaskAlreadySubmitted
    let fast_err = rpc_err(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &cls_args,
    )
    .await;
    assert_eq!(fast_err.code, ERR_PD_TOO_FAST);

    ctx.advance_secs(12);
    let dup_err = rpc_err(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &cls_args,
    )
    .await;
    assert_eq!(dup_err.code, ERR_PD_ALREADY_SUBMITTED);

    // 5. Inaccurate classification against /pd consensus 2 false yields kredits_reward == 0 and no AK notify
    common::slash_cmd(&ctx, &mut gm, "/pd consensus 2 false").await;
    let t2 = rpc_ok(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[]).await;
    let t2_id = kv_get(&t2, "taskID")
        .or_else(|| kv_get(&t2, "task_id"))
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();
    let _ = drain_all_notifications(&ctx, &mut sess, char_id);
    let miss_res = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::String(t2_id), PyRep::List(vec![PyRep::Real(2.0)])],
    )
    .await;
    let miss_ak = miss_res
        .dict_get("kredits_reward")
        .and_then(PyRep::as_i64)
        .or_else(|| find_tuple_i64(&miss_res, "AK_Reward"))
        .unwrap_or(-1);
    assert_eq!(miss_ak, 0);
    assert_eq!(
        rpc_ok(
            &ctx,
            &mut sess,
            "ProjectDiscovery",
            "get_player_analysis_kredits",
            &[],
        )
        .await
        .as_i64(),
        Some(ak_after)
    );
    let no_ak_notifs = drain_all_notifications(&ctx, &mut sess, char_id);
    assert!(
        !no_ak_notifs.iter().any(|n| n.0 == "OnAnalysisKreditsChange"),
        "Inaccurate sample with AK_Reward == 0 must not emit OnAnalysisKreditsChange"
    );

    // 6. 24-hour bonus refill & SQLite reload persistence
    ctx.advance_secs(86_401);
    assert_eq!(
        rpc_ok(
            &ctx,
            &mut sess,
            "ProjectDiscovery",
            "get_remaining_bonus_samples",
            &[],
        )
        .await
        .as_i64(),
        Some(10)
    );

    let ctx2 = reload_ctx_from_sqlite(&ctx);
    let (mut sess2, _) =
        login_existing_player(&ctx2, 60_013, "pd_cls_user", "pd_cls_user-secret", char_id).await;
    assert_eq!(
        rpc_ok(
            &ctx2,
            &mut sess2,
            "ProjectDiscovery",
            "get_player_analysis_kredits",
            &[],
        )
        .await
        .as_i64(),
        Some(ak_after)
    );
    assert_eq!(
        rpc_ok(
            &ctx2,
            &mut sess2,
            "ProjectDiscovery",
            "get_remaining_bonus_samples",
            &[],
        )
        .await
        .as_i64(),
        Some(10)
    );
}

#[tokio::test]
async fn fast_checkout_mining_details_and_plex_conservation_lifecycle() {
    // parity: rpc:FastCheckoutService.RequestMiningDetails, rpc:FastCheckoutService.RequestMiningCycleOutput, rpc:FastCheckoutService.GetOffersForUser, rpc:FastCheckoutService.BuyOffer
    eve_testkit::attest(&[
        "rpc:FastCheckoutService.RequestMiningDetails",
        "rpc:FastCheckoutService.RequestMiningCycleOutput",
        "rpc:FastCheckoutService.GetOffersForUser",
        "rpc:FastCheckoutService.BuyOffer",
    ]);

    let ctx = fresh_ctx_full_market();
    let (mut sess1, cid1, _ship1) =
        setup_pilot_in_full_market(&ctx, 60_021, "fc_miner1", "Metenox Owner").await;
    let (mut sess2, _cid2, _) =
        setup_pilot_in_full_market(&ctx, 60_022, "fc_outsider", "Outsider Pilot").await;
    let mut gm = login_demo_gm(&ctx, 60_023).await;

    // 1. Pilot 1 trains Corporation Management (3363) & Charon (20185), creates corp, deploys Metenox (81_826)
    ensure_skills_for_types(&ctx, &mut sess1, cid1, JITA_STATION, &[3363, 20185]).await;
    ctx.advance_secs(15);
    let corp_rep = rpc_ok(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Metenox Ops Corp".into()),
            PyRep::String("MTNX".into()),
            PyRep::Real(0.05),
        ],
    )
    .await;
    let corp_id = corp_rep
        .dict_get("corporationID")
        .and_then(PyRep::as_i64)
        .expect("corporationID");
    sess1.corporation_id = Some(corp_id);

    // Advance past 7-day corp age anchoring restriction
    ctx.advance_secs(7 * 86_400 + 60);

    // Acquire Metenox Moon Drill (81_826), Nitrogen Fuel Blocks (4051), & Magmatic Gas (81_143) in Jita 4-4
    let drill_item =
        gm_supply_and_buy(&ctx, &mut gm, &mut sess1, cid1, JITA_STATION, 81_826, 1).await;
    let fuel_item =
        gm_supply_and_buy(&ctx, &mut gm, &mut sess1, cid1, JITA_STATION, 4051, 200).await;
    let gas_item =
        gm_supply_and_buy(&ctx, &mut gm, &mut sess1, cid1, JITA_STATION, 81_143, 100).await;

    // Pick a lowsec system near Jita that has a moon in SDE, and choose a clear anchor spot
    // outside moon radius + MIN_ANCHOR_DISTANCE_M (1,000 km) and all other celestials/stations.
    let find_clear_moon_anchor = |sys_id: u32| -> Option<[f64; 3]> {
        let celestials = ctx.sde.celestials_in_system(sys_id);
        let stations: Vec<_> = ctx
            .sde
            .stations
            .values()
            .filter(|s| s.solar_system_id == sys_id)
            .collect();
        for moon in celestials.iter().filter(|c| c.group_id == 8) {
            let offset = moon.radius + 1_500_000.0;
            for dir in [
                glam::DVec3::new(offset, 0.0, 0.0),
                glam::DVec3::new(0.0, offset, 0.0),
                glam::DVec3::new(0.0, 0.0, offset),
            ] {
                let candidate = moon.position + dir;
                let st_ok = stations
                    .iter()
                    .all(|s| s.position.distance(candidate) >= 1_100_000.0);
                let cel_ok = celestials.iter().all(|c| match c.group_id {
                    7 | 8 | 9 | 10 => (c.position.distance(candidate) - c.radius) >= 1_100_000.0,
                    _ => true,
                });
                if st_ok && cel_ok {
                    return Some(candidate.to_array());
                }
            }
        }
        None
    };
    let (lowsec_sys, anchor_pos) = find_clear_moon_anchor(LOWSEC_NOURVUKAIKEN_SYS)
        .map(|p| (LOWSEC_NOURVUKAIKEN_SYS, p))
        .unwrap_or_else(|| {
            ctx.sde
                .solar_systems
                .values()
                .filter(|s| {
                    s.solar_system_id < 31_000_000
                        && s.region_id != 10_000_070
                        && s.security_status > 0.0
                        && s.security_status < 0.45
                })
                .find_map(|s| find_clear_moon_anchor(s.solar_system_id).map(|p| (s.solar_system_id, p)))
                .expect("lowsec moon with clear anchor point in SDE")
        });

    let charon_id = buy_item_valid_tick(&ctx, &mut sess1, cid1, JITA_STATION, 20_185, 1).await;
    ctx.advance_secs(11);
    rpc_ok(
        &ctx,
        &mut sess1,
        "ship",
        "AssembleShip",
        &[PyRep::List(vec![PyRep::Integer(charon_id)])],
    )
    .await;
    rpc_ok(&ctx, &mut sess1, "ship", "Board", &[PyRep::Integer(charon_id)]).await;
    ctx.advance_secs(11);
    sess1.ship_id = Some(charon_id);
    let active_ship = charon_id;
    for item_id in [drill_item, fuel_item, gas_item] {
        rpc_ok(
            &ctx,
            &mut sess1,
            "invbroker",
            "Add",
            &[
                PyRep::Integer(item_id),
                PyRep::Integer(active_ship),
                PyRep::Integer(5),
            ],
        )
        .await;
    }
    undock_from_station(&ctx, &mut sess1, JITA_SYS).await;
    fly_stargates_to_fast(&ctx, &mut sess1, lowsec_sys).await;
    let sys_h = ctx.systems.get_or_spawn(lowsec_sys);
    let _ = sys_h
        .set_ball_position(
            active_ship,
            glam::DVec3::from_array(anchor_pos) - glam::DVec3::new(60_000.0, 0.0, 0.0),
        )
        .await;
    ctx.advance_secs(11);

    let anchor_res = rpc_ok(
        &ctx,
        &mut sess1,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(drill_item),
            PyRep::List(vec![
                PyRep::Real(anchor_pos[0]),
                PyRep::Real(anchor_pos[1]),
                PyRep::Real(anchor_pos[2]),
            ]),
            PyRep::String("Live Metenox Drill".into()),
        ],
    )
    .await;
    let structure_id = anchor_res
        .dict_get("structureID")
        .and_then(PyRep::as_i64)
        .expect("anchored structureID");

    // Advance past AnchorVulnerable (900s) + Anchoring (86400s) + Onlining (900s), then move within 5 km to load fuel
    ctx.advance_secs(900 + 86_400 + 900 + 60);
    let _ = sys_h
        .set_ball_position(
            active_ship,
            glam::DVec3::from_array(anchor_pos) + glam::DVec3::new(1_000.0, 0.0, 0.0),
        )
        .await;
    for bay_item in [fuel_item, gas_item] {
        rpc_ok(
            &ctx,
            &mut sess1,
            "invbroker",
            "Add",
            &[
                PyRep::Integer(bay_item),
                PyRep::Integer(structure_id),
                PyRep::Integer(i64::from(InventoryFlag::SpecializedFuelBay as u16)),
            ],
        )
        .await;
    }

    // 2. RequestMiningCycleOutput & RequestMiningDetails (before and after 1h cycle)
    let cycle_rep = rpc_ok(
        &ctx,
        &mut sess1,
        "FastCheckoutService",
        "RequestMiningCycleOutput",
        &[PyRep::Integer(structure_id)],
    )
    .await;
    let PyRep::List(ref mats) = cycle_rep else {
        panic!("Expected PyRep::List from RequestMiningCycleOutput, got {cycle_rep:?}");
    };
    assert!(!mats.is_empty(), "RequestMiningCycleOutput must return materials");

    let details_before = rpc_ok(
        &ctx,
        &mut sess1,
        "FastCheckoutService",
        "RequestMiningDetails",
        &[PyRep::Integer(structure_id)],
    )
    .await;
    assert_eq!(
        details_before.dict_get("availableFuel").and_then(PyRep::as_i64),
        Some(200)
    );
    let cap_before = details_before
        .dict_get("remainingOutputBayCapacity")
        .and_then(PyRep::as_f64)
        .expect("remainingOutputBayCapacity");
    assert!(cap_before > 0.0);

    // Advance 1 hour and confirm fuel consumption & output bay usage
    ctx.advance_secs(3_600);
    let details_after = rpc_ok(
        &ctx,
        &mut sess1,
        "FastCheckoutService",
        "RequestMiningDetails",
        &[PyRep::Integer(structure_id)],
    )
    .await;
    let fuel_after = details_after
        .dict_get("availableFuel")
        .and_then(PyRep::as_i64)
        .expect("availableFuel after 1h");
    assert_eq!(fuel_after, 195);
    let cap_after = details_after
        .dict_get("remainingOutputBayCapacity")
        .and_then(PyRep::as_f64)
        .unwrap();
    assert!(cap_after < cap_before);

    // Outsider pilot is rejected with AccessDenied
    let denied = rpc_err(
        &ctx,
        &mut sess2,
        "FastCheckoutService",
        "RequestMiningDetails",
        &[PyRep::Integer(structure_id)],
    )
    .await;
    assert_eq!(denied.code, "AccessDenied");

    // 3. FastCheckout GetOffersForUser & BuyOffer with production PLEX conservation (`free_fast_checkout_plex = false`)
    let prod_ctx = reload_ctx_from_sqlite(&ctx);
    prod_ctx
        .feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .free_fast_checkout_plex = false;
    prod_ctx
        .global_config_values
        .write()
        .unwrap()
        .insert("fastCheckoutEnabled".into(), PyRep::Bool(true));
    let (mut sess_prod, _) =
        login_existing_player(&prod_ctx, 60_024, "fc_miner1", "fc_miner1-secret", cid1).await;
    // Also check mining details survived SQLite reload
    let details_reload = rpc_ok(
        &prod_ctx,
        &mut sess_prod,
        "FastCheckoutService",
        "RequestMiningDetails",
        &[PyRep::Integer(structure_id)],
    )
    .await;
    assert_eq!(
        details_reload.dict_get("availableFuel").and_then(PyRep::as_i64),
        Some(195)
    );

    let offers_rep = rpc_ok(
        &prod_ctx,
        &mut sess_prod,
        "FastCheckoutService",
        "GetOffersForUser",
        &[],
    )
    .await;
    assert!(offers_rep.dict_get("offers").is_some());

    // Wrong password -> InvalidPassword
    let bad_pw = rpc_err(
        &prod_ctx,
        &mut sess_prod,
        "FastCheckoutService",
        "BuyOffer",
        &[
            PyRep::Integer(4),
            PyRep::Integer(1),
            PyRep::String("wrong-password".into()),
        ],
    )
    .await;
    assert_eq!(bad_pw.code, "InvalidPassword");

    // Missing quick-pay token in production mode -> PaymentTokenRequired (zero free PLEX minted)
    let plex_before = prod_ctx.db.with_snapshot(|s| s.plex_vault_balance(cid1));
    let no_tok = rpc_err(
        &prod_ctx,
        &mut sess_prod,
        "FastCheckoutService",
        "BuyOffer",
        &[
            PyRep::Integer(4),
            PyRep::Integer(1),
            PyRep::String("fc_miner1-secret".into()),
        ],
    )
    .await;
    assert_eq!(no_tok.code, "PaymentTokenRequired");
    assert_eq!(
        prod_ctx.db.with_snapshot(|s| s.plex_vault_balance(cid1)),
        plex_before
    );

    // Bad offer ID -> OfferNotFound
    let bad_offer = rpc_err(
        &prod_ctx,
        &mut sess_prod,
        "FastCheckoutService",
        "BuyOffer",
        &[
            PyRep::Integer(999_999),
            PyRep::Integer(1),
            PyRep::String("fc_miner1-secret".into()),
        ],
    )
    .await;
    assert_eq!(bad_offer.code, "OfferNotFound");

    // Catalog offer 1001 (500 PLEX for 30d Omega) fails when vault has < 500 PLEX.
    // Fly back from lowsec to Jita, dock, buy 400 PLEX on Jita market (auto-settles to vault -> still < 500),
    // verify UserError, then buy 200 more PLEX (+600 total) and complete BuyOffer(1001) leaving +100 PLEX.
    sess_prod.ship_id = Some(active_ship);
    fly_stargates_to_fast(&prod_ctx, &mut sess_prod, JITA_SYS).await;
    warp_and_dock_at_station_fast(&prod_ctx, &mut sess_prod, active_ship, JITA_SYS, JITA_STATION).await;
    let plex_ask = prod_ctx
        .db
        .with_snapshot(|s| {
            s.market_orders
                .values()
                .filter(|o| {
                    !o.is_bid && o.station_id == JITA_STATION && o.type_id == 44_992 && o.vol_remaining > 0
                })
                .map(|o| o.price.to_f64_isk())
                .reduce(f64::min)
        })
        .unwrap_or(5_300_000.0);
    let raw_px = (plex_ask * 1.15).ceil();
    let exp_px = (raw_px.log10().floor() as i32 - 2).max(0);
    let step_px = 10.0_f64.powi(exp_px);
    let plex_buy_price = (raw_px / step_px).ceil() * step_px;

    rpc_ok(
        &prod_ctx,
        &mut sess_prod,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(44_992),
            PyRep::Real(plex_buy_price),
            PyRep::Integer(400),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
    )
    .await;
    assert_eq!(
        prod_ctx.db.with_snapshot(|s| s.plex_vault_balance(cid1)),
        plex_before + 400
    );
    let over_qty = (plex_before + 400) / 500 + 1;
    let ins_err = rpc_err(
        &prod_ctx,
        &mut sess_prod,
        "FastCheckoutService",
        "BuyOffer",
        &[
            PyRep::Integer(1001),
            PyRep::Integer(over_qty),
            PyRep::String("fc_miner1-secret".into()),
        ],
    )
    .await;
    assert_eq!(ins_err.code, "UserError");

    // Buy remaining 200 PLEX (total vault delta +600), then buy offer 1001 (cost 500 PLEX)
    rpc_ok(
        &prod_ctx,
        &mut sess_prod,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(44_992),
            PyRep::Real(plex_buy_price),
            PyRep::Integer(200),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
    )
    .await;
    let expected_after = plex_before + 600 - 500;
    let buy_ok = rpc_ok(
        &prod_ctx,
        &mut sess_prod,
        "FastCheckoutService",
        "BuyOffer",
        &[
            PyRep::Integer(1001),
            PyRep::Integer(1),
            PyRep::String("fc_miner1-secret".into()),
        ],
    )
    .await;
    assert_eq!(
        buy_ok.dict_get("newPlexBalance").and_then(PyRep::as_i64),
        Some(expected_after)
    );
    let prod_ctx2 = reload_ctx_from_sqlite(&prod_ctx);
    assert_eq!(
        prod_ctx2.db.with_snapshot(|s| s.plex_vault_balance(cid1)),
        expected_after
    );
}

#[tokio::test]
async fn nes_store_sde_catalog_plex_purchase_and_reload() {
    // parity: feat:wallet.nes-store, rpc:storeManager.get_categories, rpc:storeManager.get_products, rpc:storeManager.get_offers, rpc:storeManager.can_purchase_offer, rpc:storeManager.buy_offer
    eve_testkit::attest(&[
        "feat:wallet.nes-store",
        "rpc:storeManager.get_categories",
        "rpc:storeManager.get_products",
        "rpc:storeManager.get_offers",
        "rpc:storeManager.can_purchase_offer",
        "rpc:storeManager.buy_offer",
    ]);

    let ctx = fresh_player_ctx();
    let (mut sess, char_id, _) =
        create_player_pilot_fast(&ctx, 60_031, "nes_buyer", "NES Shopper", JITA_SYS).await;
    assert_eq!(sess.role & service_roles::ROLE_GM_MASK, 0);

    let cats = rpc_ok(&ctx, &mut sess, "storeManager", "get_categories", &[]).await;
    let prods = rpc_ok(&ctx, &mut sess, "storeManager", "get_products", &[]).await;
    let offers = rpc_ok(&ctx, &mut sess, "storeManager", "get_offers", &[]).await;
    let PyRep::List(ref offer_list) = offers else {
        panic!("Expected list from storeManager.get_offers, got {offers:?}");
    };
    let PyRep::List(ref cat_list) = cats else {
        panic!("Expected list from storeManager.get_categories, got {cats:?}");
    };
    let PyRep::List(ref prod_list) = prods else {
        panic!("Expected list from storeManager.get_products, got {prods:?}");
    };
    assert!(cat_list.len() >= 3);
    assert!(prod_list.len() >= 20, "SDE SKINs must populate NES catalog");
    assert!(offer_list.len() >= 20, "SDE SKIN offers must populate NES catalog");

    // Pick first published Ship SKIN offer (>= 2001) and inspect its PLEX price & SKIN type_id
    let skin_offer = offer_list
        .iter()
        .find(|o| o.dict_get("id").and_then(PyRep::as_i64).unwrap_or(0) >= 2001)
        .expect("SDE Ship SKIN offer >= 2001");
    let skin_offer_id = skin_offer.dict_get("id").and_then(PyRep::as_i64).unwrap();
    let skin_price = skin_offer
        .dict_get("plexPrice")
        .or_else(|| skin_offer.dict_get("price"))
        .or_else(|| skin_offer.dict_get("pricePlex"))
        .or_else(|| skin_offer.dict_get("price_plex"))
        .and_then(PyRep::as_i64)
        .expect("skin offer plexPrice");
    assert!(skin_price > 0);

    // 1. Negative paths: insufficient vault balance, removed AUR currency, invalid currency
    let vault_before = ctx.db.with_snapshot(|s| s.plex_vault_balance(char_id));
    let over_qty = (vault_before / skin_price) + 1;
    assert_eq!(
        rpc_ok(
            &ctx,
            &mut sess,
            "storeManager",
            "can_purchase_offer",
            &[
                PyRep::Integer(skin_offer_id),
                PyRep::String("PLX".into()),
                PyRep::Integer(over_qty),
            ],
        )
        .await,
        PyRep::Bool(false)
    );
    let aur_err = rpc_err(
        &ctx,
        &mut sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(skin_offer_id), PyRep::String("AUR".into())],
    )
    .await;
    assert_eq!(aur_err.code, "AurumCurrencyRemoved");

    let cur_err = rpc_err(
        &ctx,
        &mut sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(skin_offer_id), PyRep::String("ISK".into())],
    )
    .await;
    assert_eq!(cur_err.code, "InvalidCurrency");

    let ins_err = rpc_err(
        &ctx,
        &mut sess,
        "storeManager",
        "buy_offer",
        &[
            PyRep::Integer(skin_offer_id),
            PyRep::String("PLX".into()),
            PyRep::Integer(over_qty),
        ],
    )
    .await;
    assert_eq!(ins_err.code, "InsufficientPlexBalance");

    // 2. Buy real PLEX on Jita market (automatically settles to account PLEX Vault)
    let plex_qty = skin_price + 500;
    let plex_ask = ctx
        .db
        .with_snapshot(|s| {
            s.market_orders
                .values()
                .filter(|o| {
                    !o.is_bid && o.station_id == JITA_STATION && o.type_id == 44_992 && o.vol_remaining > 0
                })
                .map(|o| o.price.to_f64_isk())
                .reduce(f64::min)
        })
        .unwrap_or(5_300_000.0);
    let raw_px = (plex_ask * 1.15).ceil();
    let exp_px = (raw_px.log10().floor() as i32 - 2).max(0);
    let step_px = 10.0_f64.powi(exp_px);
    let plex_buy_price = (raw_px / step_px).ceil() * step_px;
    rpc_ok(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(44_992),
            PyRep::Real(plex_buy_price),
            PyRep::Integer(plex_qty),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
    )
    .await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.plex_vault_balance(char_id)),
        vault_before + plex_qty
    );
    assert_eq!(
        rpc_ok(
            &ctx,
            &mut sess,
            "storeManager",
            "can_purchase_offer",
            &[PyRep::Integer(skin_offer_id)],
        )
        .await,
        PyRep::Bool(true)
    );
    let _ = drain_all_notifications(&ctx, &mut sess, char_id);

    // 3. Purchase SDE SKIN offer + Omega (1001), verify PLEX conservation, item delivery, Omega & OnAccountChange
    let buy_skin = rpc_ok(
        &ctx,
        &mut sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(skin_offer_id), PyRep::String("PLX".into()), PyRep::Integer(1)],
    )
    .await;
    let delivered_item_id = kv_get(&buy_skin, "deliveredItemID")
        .or_else(|| kv_get(&buy_skin, "itemID"))
        .or_else(|| buy_skin.dict_get("item_id"))
        .and_then(PyRep::as_i64)
        .unwrap_or(0);
    assert!(delivered_item_id > 0);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.plex_vault_balance(char_id)),
        vault_before + 500
    );

    let _buy_omega = rpc_ok(
        &ctx,
        &mut sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(1001), PyRep::String("PLX".into()), PyRep::Integer(1)],
    )
    .await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.plex_vault_balance(char_id)),
        vault_before
    );
    assert_eq!(ctx.db.get_character(char_id).unwrap().clone_grade, 2);

    let notifs = drain_all_notifications(&ctx, &mut sess, char_id);
    assert!(
        notifs.iter().any(|n| n.0 == "OnAccountChange"),
        "OnAccountChange must fire on PLEX NES purchase, got {notifs:?}"
    );

    // 4. SQLite reload preserves vault balance, SKIN item, and Omega clone grade
    let ctx2 = reload_ctx_from_sqlite(&ctx);
    assert_eq!(
        ctx2.db.with_snapshot(|s| s.plex_vault_balance(char_id)),
        vault_before
    );
    assert_eq!(ctx2.db.get_character(char_id).unwrap().clone_grade, 2);
    assert!(ctx2
        .db
        .with_snapshot(|s| s.items.get(&delivered_item_id).is_some_and(|i| i.owner_id == char_id)));
}

#[tokio::test]
async fn login_campaign_downtime_redeem_and_enrollment_isolation() {
    // parity: feat:pve.login-campaign, rpc:loginCampaignManager.get_client_campaign_state, rpc:loginCampaignManager.claim_reward, notify:OnCampaignEnrollmentChanged
    eve_testkit::attest(&[
        "feat:pve.login-campaign",
        "rpc:loginCampaignManager.get_client_campaign_state",
        "rpc:loginCampaignManager.claim_reward",
        "notify:OnCampaignEnrollmentChanged",
    ]);

    let ctx = fresh_player_ctx();
    let (mut sess_a, char_a, _) =
        create_player_pilot_fast(&ctx, 60_041, "lc_pilot_a", "Docked Campaign Pilot", JITA_SYS).await;
    let (mut sess_b, char_b, _) =
        create_player_pilot_fast(&ctx, 60_042, "lc_pilot_b", "Space Campaign Pilot", JITA_SYS).await;
    let mut gm = login_demo_gm(&ctx, 60_043).await;

    // Undock pilot B so its login reward routes to the Redeeming Queue instead of station hangar
    undock_from_station(&ctx, &mut sess_b, JITA_SYS).await;

    // 1. Query initial state for docked pilot A -> default SDE campaign 2261, 30 days, claimable today
    let campaign_id_of = |st: &PyRep| {
        kv_get(st, "static_data")
            .and_then(|sd| kv_get(sd, "campaign_id"))
            .and_then(PyRep::as_i64)
    };
    let next_ts_of = |st: &PyRep| {
        kv_get(st, "item_progress")
            .and_then(|ip| kv_get(ip, "next_reward_timestamp"))
            .and_then(PyRep::as_i64)
    };
    let state_a0 = rpc_ok(
        &ctx,
        &mut sess_a,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await;
    let camp_id_a0 = campaign_id_of(&state_a0).expect("campaign_id in static_data");
    assert!(
        camp_id_a0 == 2347 || camp_id_a0 == 2261,
        "Expected active SDE login campaign (rookie 2347 or default 2261), got {camp_id_a0}"
    );
    assert_eq!(next_ts_of(&state_a0), Some(0));

    let _ = drain_all_notifications(&ctx, &mut sess_a, char_a);
    let _ = drain_all_notifications(&ctx, &mut sess_b, char_b);

    // 2. Claim reward while docked -> spawns item in Jita hangar & emits OnItemChange;
    //    duplicate claim on same EVE downtime day raises DailyRewardAlreadyClaimedToday
    let claim_a = rpc_ok(&ctx, &mut sess_a, "loginCampaignManager", "claim_reward", &[]).await;
    let PyRep::Tuple(ref c_tup) = claim_a else {
        panic!("Expected 4-tuple from loginCampaignManager.claim_reward, got {claim_a:?}");
    };
    let reward_tid = kv_get(&c_tup[0], "typeID")
        .and_then(PyRep::as_i64)
        .expect("reward typeID") as u32;
    assert!(
        count_hangar_items_of_type(&ctx, char_a, JITA_STATION, reward_tid) >= 1,
        "Docked login reward must appear in station hangar"
    );
    let notifs_a = drain_all_notifications(&ctx, &mut sess_a, char_a);
    assert!(notifs_a.iter().any(|n| n.0 == "OnItemChange"));

    let dup_claim = rpc_err(&ctx, &mut sess_a, "loginCampaignManager", "claim_reward", &[]).await;
    assert_eq!(dup_claim.code, ERR_DAILY_ALREADY_CLAIMED);

    let state_a1 = rpc_ok(
        &ctx,
        &mut sess_a,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await;
    assert!(next_ts_of(&state_a1).unwrap_or(0) > 0);

    // 3. Undocked pilot B claims reward -> delivered to account redeeming queue + OnRedeemingTokensUpdated
    let _ = rpc_ok(&ctx, &mut sess_b, "loginCampaignManager", "claim_reward", &[]).await;
    let notifs_b = drain_all_notifications(&ctx, &mut sess_b, char_b);
    assert!(notifs_b
        .iter()
        .any(|n| n.0 == "OnRedeemingTokensUpdated"));
    let tokens_b = rpc_ok(&ctx, &mut sess_b, "userSvc", "GetRedeemTokens", &[]).await;
    let PyRep::List(ref tok_list) = tokens_b else {
        panic!("Expected list from GetRedeemTokens, got {tokens_b:?}");
    };
    assert!(!tok_list.is_empty(), "Undocked claim must enqueue redeem token");

    // 4. Per-character campaign enrollment emits OnCampaignEnrollmentChanged to pilot A
    //    and does NOT alter pilot B's active campaign
    common::slash_cmd(&ctx, &mut gm, &format!("/campaign enroll 2323 {char_a}")).await;
    let enroll_notifs_a = drain_all_notifications(&ctx, &mut sess_a, char_a);
    assert!(
        enroll_notifs_a
            .iter()
            .any(|n| n.0 == "OnCampaignEnrollmentChanged"),
        "OnCampaignEnrollmentChanged must arrive for enrolled pilot A, got {enroll_notifs_a:?}"
    );
    let enroll_notifs_b = drain_all_notifications(&ctx, &mut sess_b, char_b);
    assert!(
        !enroll_notifs_b
            .iter()
            .any(|n| n.0 == "OnCampaignEnrollmentChanged"),
        "Pilot B must not receive pilot A's enrollment notification"
    );

    let state_a_enrolled = rpc_ok(
        &ctx,
        &mut sess_a,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await;
    let state_b_isolated = rpc_ok(
        &ctx,
        &mut sess_b,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await;
    assert_eq!(campaign_id_of(&state_a_enrolled), Some(2323));
    assert_eq!(campaign_id_of(&state_b_isolated), Some(camp_id_a0));

    // 5. Advance clock +86_400s across 11:00 UTC downtime rollover, claim day 1 on 2323, and verify SQLite reload
    ctx.advance_secs(86_400);
    let state_next_day = rpc_ok(
        &ctx,
        &mut sess_a,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await;
    assert_eq!(next_ts_of(&state_next_day), Some(0));
    let _ = rpc_ok(&ctx, &mut sess_a, "loginCampaignManager", "claim_reward", &[]).await;

    let ctx2 = reload_ctx_from_sqlite(&ctx);
    let (mut sess_a2, _) =
        login_existing_player(&ctx2, 60_044, "lc_pilot_a", "lc_pilot_a-secret", char_a).await;
    let state_reload = rpc_ok(
        &ctx2,
        &mut sess_a2,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await;
    assert_eq!(campaign_id_of(&state_reload), Some(2323));
    assert!(next_ts_of(&state_reload).unwrap_or(0) > 0);
}

#[tokio::test]
async fn agent_mission_completion_isk_lp_standing_and_reload() {
    // parity: feat:pve.mission-complete, plan:E-5
    eve_testkit::attest(&["feat:pve.mission-complete", "plan:E-5"]);

    let ctx = fresh_player_ctx();
    let (mut sess, char_id, _) =
        create_player_pilot_fast(&ctx, 60_051, "mission_pilot", "Mission Runner", JITA_SYS).await;
    assert_eq!(sess.role & service_roles::ROLE_GM_MASK, 0);

    // 1. Standing / Alpha gate rejects L4 agent offer request or accept
    let l4_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.solar_system_id == Some(JITA_SYS) && a.station_id == Some(JITA_STATION) && a.level == 4)
        .map(|a| a.agent_id);
    if let Some(l4_id) = l4_agent {
        let l4_oid = bind_service(&ctx, &mut sess, "agentMgr", PyRep::Integer(l4_id)).await as u64;
        if let Ok(offer) = ctx
            .dispatch_rpc(
                &mut sess,
                "agentMgr",
                Some(l4_oid),
                "DoAction",
                &[PyRep::Integer(2)],
                &HashMap::new(),
                None,
            )
            .await
        {
            if offer.dict_get("missionID").is_some() {
                let acc_err = ctx
                    .dispatch_rpc(
                        &mut sess,
                        "agentMgr",
                        Some(l4_oid),
                        "DoAction",
                        &[PyRep::Integer(3)],
                        &HashMap::new(),
                        None,
                    )
                    .await
                    .expect_err("Alpha/low-standing pilot must not accept L4 mission");
                assert!(!acc_err.code.is_empty());
            }
        }
    }

    // 2. Find an L1 Security agent via agentMgr.GetAgents(1, 24) (in Jita or nearest hub)
    let agents_rep = rpc_ok(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetAgents",
        &[PyRep::Integer(1), PyRep::Integer(24)],
    )
    .await;
    let PyRep::Rowset { rows: ref l1_rows, .. } = agents_rep else {
        panic!("Expected Rowset from agentMgr.GetAgents, got {agents_rep:?}");
    };
    assert!(!l1_rows.is_empty());
    let agent_row = l1_rows
        .iter()
        .find(|r| r[5].as_i64() == Some(i64::from(JITA_SYS)) && r[6].as_i64() == Some(JITA_STATION))
        .or_else(|| l1_rows.iter().find(|r| r[5].as_i64() == Some(i64::from(JITA_SYS))))
        .unwrap_or(&l1_rows[0]);
    let l1_agent_id = agent_row[0].as_i64().unwrap();
    let agent_corp_id = agent_row[3].as_i64().unwrap();
    let agent_sys = agent_row[5]
        .as_i64()
        .unwrap_or(i64::from(JITA_SYS)) as eve_sde::SolarSystemId;
    let agent_station = agent_row[6].as_i64().unwrap_or(JITA_STATION);

    // Fit a combat Omen in Jita and fly to the agent's station if outside Jita 4-4
    let (combat_ship_id, guns) =
        buy_fit_combat_ship(&ctx, &mut sess, char_id, JITA_STATION, 2006, 486, 201, 5).await;
    if agent_station != JITA_STATION {
        if agent_sys != JITA_SYS {
            undock_from_station(&ctx, &mut sess, JITA_SYS).await;
            fly_stargates_to_fast(&ctx, &mut sess, agent_sys).await;
        } else {
            undock_from_station(&ctx, &mut sess, JITA_SYS).await;
        }
        warp_and_dock_at_station_fast(&ctx, &mut sess, combat_ship_id, agent_sys, agent_station).await;
    }

    let agent_oid =
        bind_service(&ctx, &mut sess, "agentMgr", PyRep::Integer(l1_agent_id)).await as u64;
    let offer = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            Some(agent_oid),
            "DoAction",
            &[PyRep::Integer(2)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("DoAction(2) request L1 mission");
    let mission_id = offer
        .dict_get("missionID")
        .and_then(PyRep::as_i64)
        .expect("missionID") as u32;
    let isk_reward = offer
        .dict_get("iskReward")
        .and_then(PyRep::as_i64)
        .unwrap_or(0);
    let bonus_reward = offer
        .dict_get("bonusReward")
        .and_then(PyRep::as_i64)
        .unwrap_or(0);
    let lp_reward = offer
        .dict_get("lpReward")
        .and_then(PyRep::as_i64)
        .unwrap_or(0);
    assert!(isk_reward > 0 && lp_reward > 0);

    // Accept offer
    ctx.dispatch_rpc(
        &mut sess,
        "agentMgr",
        Some(agent_oid),
        "DoAction",
        &[PyRep::Integer(3)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("DoAction(3) accept mission");

    let isk_before = ctx.db.get_character(char_id).unwrap().balance.to_f64_isk();
    let lp_before = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).and_then(|c| c.loyalty_points.get(&agent_corp_id).copied()).unwrap_or(0));
    let standing_before = ctx.db.with_snapshot(|s| {
        s.characters
            .get(&char_id)
            .and_then(|c| c.standings.get(&l1_agent_id).copied())
            .unwrap_or(0.0)
    });

    // Check if mission spawned pocket NPCs or assigned courier destination
    let (spawned_balls, dest_station_opt) = ctx.db.with_snapshot(|s| {
        let m = s.active_missions.get(&(char_id, mission_id)).cloned();
        let meta = s
            .owner_notes
            .values()
            .find(|n| n.character_id == char_id && n.label == format!("mission_meta:{mission_id}"))
            .and_then(|n| serde_json::from_str::<serde_json::Value>(&n.note).ok());
        let dest = meta.and_then(|v| v.get("destination_station_id").and_then(|x| x.as_i64()));
        (m.map(|x| x.spawned_npc_ball_ids).unwrap_or_default(), dest)
    });

    // Negative check: trying DoAction(6) before clearing objectives or remotely when in space fails
    if !spawned_balls.is_empty() {
        let early_complete = ctx
            .dispatch_rpc(
                &mut sess,
                "agentMgr",
                Some(agent_oid),
                "DoAction",
                &[PyRep::Integer(6)],
                &HashMap::new(),
                None,
            )
            .await;
        assert!(
            early_complete.is_err(),
            "Mission with live NPCs must not complete early"
        );

        undock_from_station(&ctx, &mut sess, agent_sys).await;
        let active_ship = combat_ship_id;
        for ball_id in spawned_balls {
            warp_and_engage_ball(
                &ctx,
                &mut sess,
                agent_sys,
                active_ship,
                &guns,
                ball_id,
                "item",
                PyRep::Integer(ball_id),
            )
            .await;
        }
        warp_and_dock_at_station_fast(&ctx, &mut sess, active_ship, agent_sys, agent_station).await;
    } else if let Some(dest_sid) = dest_station_opt {
        if dest_sid != agent_station {
            // Try completing remotely at wrong station first -> must reject
            let remote_err = ctx
                .dispatch_rpc(
                    &mut sess,
                    "agentMgr",
                    Some(agent_oid),
                    "DoAction",
                    &[PyRep::Integer(6)],
                    &HashMap::new(),
                    None,
                )
                .await;
            assert!(remote_err.is_err());
            let dest_sys = ctx
                .sde
                .stations
                .get(&dest_sid)
                .map(|s| s.solar_system_id)
                .unwrap_or(agent_sys);
            if sess.station_id.is_some() {
                undock_from_station(&ctx, &mut sess, agent_sys).await;
            }
            fly_stargates_to_fast(&ctx, &mut sess, dest_sys).await;
            warp_and_dock_at_station_fast(&ctx, &mut sess, combat_ship_id, dest_sys, dest_sid).await;
        }
    }

    // Complete mission!
    let _complete = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            Some(agent_oid),
            "DoAction",
            &[PyRep::Integer(6)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("Complete mission after fulfilling objectives");

    let isk_after = ctx.db.get_character(char_id).unwrap().balance.to_f64_isk();
    let lp_after = ctx.db.with_snapshot(|s| {
        s.characters
            .get(&char_id)
            .and_then(|c| c.loyalty_points.get(&agent_corp_id).copied())
            .unwrap_or(0)
    });
    let standing_after = ctx.db.with_snapshot(|s| {
        s.characters
            .get(&char_id)
            .and_then(|c| c.standings.get(&l1_agent_id).copied())
            .unwrap_or(0.0)
    });
    assert!(
        isk_after > isk_before && isk_after >= isk_before + (isk_reward as f64) * 0.80,
        "Mission completion must pay ISK ({isk_reward} + bonus {bonus_reward}, minus NPC corp tax): {isk_before} -> {isk_after}"
    );
    let min_net_lp = if isk_reward >= 100_000 {
        lp_reward - ((lp_reward * 11 + 50) / 100)
    } else {
        lp_reward
    };
    assert!(
        lp_after >= lp_before + min_net_lp.max(1),
        "LP must increase by net {min_net_lp} (gross {lp_reward}): {lp_before} -> {lp_after}"
    );
    assert!(standing_after > standing_before, "Agent standing must increase");

    // Journal must record the completion payout
    let journal_entries = ctx.db.with_snapshot(|s| {
        s.wallet_journal
            .iter()
            .filter(|j| j.character_id == char_id)
            .cloned()
            .collect::<Vec<_>>()
    });
    assert!(
        journal_entries
            .iter()
            .any(|j| j.entry_type.contains("Agent") || j.reason.contains("Mission")),
        "Wallet journal must include mission completion entry"
    );

    // SQLite reload preserves ISK, LP, standings, and removes mission from active journal
    let ctx2 = reload_ctx_from_sqlite(&ctx);
    let lp_reload = ctx2.db.with_snapshot(|s| {
        s.characters
            .get(&char_id)
            .and_then(|c| c.loyalty_points.get(&agent_corp_id).copied())
            .unwrap_or(0)
    });
    assert_eq!(lp_reload, lp_after);
    assert_eq!(
        ctx2.db.get_character(char_id).unwrap().balance.to_f64_isk(),
        isk_after
    );
}

#[tokio::test]
async fn daily_goals_evermarks_sp_rollover_and_reload() {
    // parity: feat:pve.daily-goals, feat:quasar.dailygoal
    eve_testkit::attest(&["feat:pve.daily-goals", "feat:quasar.dailygoal"]);

    let ctx = fresh_ctx_full_market();
    let (mut sess, char_id, _) =
        setup_pilot_in_full_market(&ctx, 60_061, "dg_pilot", "Daily Goal Pilot").await;
    assert_eq!(sess.role & service_roles::ROLE_GM_MASK, 0);

    // Create player corp (requires Corporation Management 3363 lvl 1 and cleared dock timer)
    ensure_skills_for_types(&ctx, &mut sess, char_id, JITA_STATION, &[3363]).await;
    ctx.advance_secs(15);
    let corp_rep = rpc_ok(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Daily Goals Corp".into()),
            PyRep::String("DGLC".into()),
            PyRep::Real(0.05),
        ],
    )
    .await;
    let corp_id = corp_rep
        .dict_get("corporationID")
        .and_then(PyRep::as_i64)
        .expect("corp_id");
    sess.corporation_id = Some(corp_id);

    let mut qrx = subscribe_quasar(&ctx, &mut sess).await;

    // 1. Fetch daily goals with rewards & current daily goals (4-of-5 rotating SDE goals + DailyBonus)
    let defs_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.GetAllWithRewardsRequest", vec![]).await;
    assert_eq!(defs_rsp.status_code, STATUS_OK);
    let mut cur_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.GetAllCurrentRequest", vec![]).await;
    assert_eq!(cur_rsp.status_code, STATUS_OK);
    let mut goal_uuids = extract_16byte_uuids(&cur_rsp.payload.unwrap().value);
    if !goal_uuids.iter().any(|u| u.starts_with(b"SDEDG") && u[14] == 0 && u[15] == 5) {
        ctx.advance_secs(86_400);
        cur_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.GetAllCurrentRequest", vec![]).await;
        assert_eq!(cur_rsp.status_code, STATUS_OK);
        goal_uuids = extract_16byte_uuids(&cur_rsp.payload.unwrap().value);
    }
    assert!(goal_uuids.len() >= 5, "Expected 4 rotating SDE daily goals + DailyBonus, got {}", goal_uuids.len());

    // Identify ManufactureItem (category 5 -> target 1), a second active regular goal (preferring MineOre category 2),
    // and DailyBonus (category 6 -> last byte 6)
    let mfg_uuid = *goal_uuids
        .iter()
        .find(|u| u.starts_with(b"SDEDG") && u[14] == 0 && u[15] == 5)
        .expect("ManufactureItem daily goal UUID");
    let mine_uuid = *goal_uuids
        .iter()
        .find(|u| u.starts_with(b"SDEDG") && u[14] == 0 && u[15] == 2)
        .or_else(|| goal_uuids.iter().find(|u| u.starts_with(b"SDEDG") && u[14] == 0 && **u != mfg_uuid && u[15] != 6))
        .expect("Second active regular daily goal UUID");
    let bonus_uuid = *goal_uuids
        .iter()
        .find(|u| u.starts_with(b"SDEDG") && u[14] == 0 && u[15] == 6)
        .expect("DailyBonus daily goal UUID");

    // 2. Non-GM admin progress rejected STATUS_FORBIDDEN; incomplete redeem rejected STATUS_FORBIDDEN
    let mut adm_req = uuid_req(mfg_uuid);
    encode_varint_field(2, 1, &mut adm_req);
    let adm_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.admin.ProgressRequest",
        adm_req,
    )
    .await;
    assert_eq!(adm_rsp.status_code, STATUS_FORBIDDEN);

    let early_redeem = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.RedeemRequest",
        uuid_req(mfg_uuid),
    )
    .await;
    assert_eq!(early_redeem.status_code, STATUS_FORBIDDEN);

    // 3. Complete ManufactureItem via REAL industry job installation + completion!
    let (bp_tid, bp_mats) = pick_small_seeded_bpo(&ctx, JITA_STATION);
    let _bpo = buy_item_valid_tick(&ctx, &mut sess, char_id, JITA_STATION, bp_tid, 1).await;
    for m in bp_mats {
        let _ = buy_item_valid_tick(&ctx, &mut sess, char_id, JITA_STATION, m.type_id, m.quantity as i64).await;
    }
    let inst = rpc_ok(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(i64::from(bp_tid)), PyRep::Integer(1), PyRep::Integer(1)],
    )
    .await;
    let job_id = inst.dict_get("jobID").and_then(PyRep::as_i64).expect("jobID");
    ctx.advance_secs(7_200);
    rpc_ok(
        &ctx,
        &mut sess,
        "industryManager",
        "CompleteJob",
        &[PyRep::Integer(job_id)],
    )
    .await;

    let notices = drain_quasar_notices(&mut qrx);
    assert!(
        notices.iter().any(|n| n.type_url.contains("dailygoal.api.ProgressedNotice")),
        "Industry job completion must emit dailygoal ProgressedNotice"
    );
    assert!(
        notices.iter().any(|n| n.type_url.contains("dailygoal.api.CompletedNotice")),
        "Industry job completion must emit dailygoal CompletedNotice"
    );

    // 4. Redeem ManufactureItem -> pays +500 EverMarks to character AND +500 EverMarks to corp!
    let em_char_before = ctx.db.with_snapshot(|s| {
        s.characters
            .get(&char_id)
            .and_then(|c| c.loyalty_points.get(&1_000_419).copied())
            .unwrap_or(0)
    });
    let em_corp_before = ctx.db.with_snapshot(|s| {
        s.corp_lp_balances
            .get(&(corp_id, 1_000_419))
            .copied()
            .unwrap_or(0)
    });
    let sp_before = ctx.db.get_character(char_id).unwrap().free_skill_points;

    let r1 = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.RedeemRequest",
        uuid_req(mfg_uuid),
    )
    .await;
    assert_eq!(r1.status_code, STATUS_OK);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].loyalty_points[&1_000_419]),
        em_char_before + 500
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.corp_lp_balances[&(corp_id, 1_000_419)]),
        em_corp_before + 500
    );
    // Double redeem rejected
    let dup_redeem = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.RedeemRequest",
        uuid_req(mfg_uuid),
    )
    .await;
    assert_eq!(dup_redeem.status_code, STATUS_FORBIDDEN);

    // 5. Fund EverMarks (2,500 EM first-paid-goal progressive cost) and complete second goal via PayForCompletionRequest -> unlocks DailyBonus -> redeem +10,000 SP!
    let mut gm = login_demo_gm(&ctx, 60_063).await;
    let mut op_id_m = Vec::new();
    encode_bytes_field(1, &[0x44u8; 16], &mut op_id_m);
    let mut wallet_id = id_payload(1, char_id);
    wallet_id.extend_from_slice(&id_payload(3, 1_000_419));
    let mut curr_m = Vec::new();
    encode_varint_field(1, 2_500, &mut curr_m);
    let mut op_attr = Vec::new();
    encode_bytes_field(1, &wallet_id, &mut op_attr);
    encode_bytes_field(2, &curr_m, &mut op_attr);
    let mut dep_req = Vec::new();
    encode_bytes_field(1, &op_id_m, &mut dep_req);
    encode_bytes_field(2, &op_attr, &mut dep_req);
    let dep_em = quasar_rpc(
        &ctx,
        &mut gm,
        "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
        dep_req,
    )
    .await;
    assert_eq!(dep_em.status_code, STATUS_OK);

    let pay_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.PayForCompletionRequest",
        uuid_req(mine_uuid),
    )
    .await;
    assert_eq!(pay_rsp.status_code, STATUS_OK);
    let r_mine = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.RedeemRequest",
        uuid_req(mine_uuid),
    )
    .await;
    assert_eq!(r_mine.status_code, STATUS_OK);
    let r_bonus = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.RedeemRequest",
        uuid_req(bonus_uuid),
    )
    .await;
    assert_eq!(r_bonus.status_code, STATUS_OK);
    let sp_after = ctx.db.get_character(char_id).unwrap().free_skill_points;
    assert!(
        sp_after >= sp_before + 10_000.0,
        "DailyBonus redemption must grant 10,000 free SP ({sp_before} -> {sp_after})"
    );

    // 6. SQLite reload preserves EverMarks, corp EverMarks, SP, and claimed status;
    //    next downtime day resets daily goals!
    let ctx2 = reload_ctx_from_sqlite(&ctx);
    let (mut sess2, _) =
        login_existing_player(&ctx2, 60_062, "dg_pilot", "dg_pilot-secret", char_id).await;
    assert_eq!(ctx2.db.get_character(char_id).unwrap().free_skill_points, sp_after);
    assert_eq!(
        ctx2.db.with_snapshot(|s| s.corp_lp_balances[&(corp_id, 1_000_419)]),
        em_corp_before + 3_500
    );
    let dup_after_reload = quasar_rpc(
        &ctx2,
        &mut sess2,
        "eve_public.dailygoal.api.RedeemRequest",
        uuid_req(bonus_uuid),
    )
    .await;
    assert_eq!(dup_after_reload.status_code, STATUS_FORBIDDEN);

    ctx2.advance_secs(86_400);
    let cur_next_day =
        quasar_rpc(&ctx2, &mut sess2, "eve_public.dailygoal.api.GetAllCurrentRequest", vec![]).await;
    assert_eq!(cur_next_day.status_code, STATUS_OK);
    let next_uuids = extract_16byte_uuids(&cur_next_day.payload.unwrap().value);
    assert!(next_uuids.len() >= 5);
}

#[tokio::test]
async fn air_career_program_progression_milestones_and_reload() {
    // parity: feat:pve.air-career-program
    eve_testkit::attest(&["feat:pve.air-career-program"]);

    let ctx = fresh_player_ctx();
    let (mut sess, char_id, _) =
        create_player_pilot_fast(&ctx, 60_071, "acp_pilot", "AIR Career Pilot", JITA_SYS).await;
    assert_eq!(sess.role & service_roles::ROLE_GM_MASK, 0);
    let mut gm = login_demo_gm(&ctx, 60_072).await;
    let mut qrx = subscribe_quasar(&ctx, &mut sess).await;

    // 1. Fetch Career Goal definitions and character goals
    let defs_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.GetDefinitionsRequest", vec![]).await;
    assert_eq!(defs_rsp.status_code, STATUS_OK);
    let defs = ctx.quasar_gateway.progression.career_goals.load_definitions();
    assert!(!defs.is_empty());

    let all_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.GetAllRequest", vec![]).await;
    assert_eq!(all_rsp.status_code, STATUS_OK);

    // Pick a goal matching "travel" / stargate jump so a live jump progresses it,
    // plus enough goals in one career to cross a graduation/CP milestone
    let travel_goal = defs
        .iter()
        .find(|d| d.activity_kind == "travel")
        .unwrap_or(&defs[0])
        .clone();

    // 2. Non-GM admin progress rejected STATUS_FORBIDDEN
    let mut non_gm_adm = uuid_req(travel_goal.uuid);
    encode_varint_field(2, 1, &mut non_gm_adm);
    assert_eq!(
        quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.career.goal.api.admin.ProgressRequest",
            non_gm_adm,
        )
        .await
        .status_code,
        STATUS_FORBIDDEN
    );

    // 3. Perform REAL stargate jump to Perimeter and back to trigger "travel" gameplay progression
    let dest_sys = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == JITA_SYS)
        .map(|j| j.to_solar_system_id)
        .unwrap_or(30_000_144);
    fly_stargates_to_fast(&ctx, &mut sess, dest_sys).await;
    let notices_after_jump = drain_quasar_notices(&mut qrx);
    assert!(
        notices_after_jump
            .iter()
            .any(|n| n.type_url.contains("career.goal.api.ProgressedNotice")),
        "Live stargate jump must emit career goal ProgressedNotice"
    );

    // Complete that career goal via GM admin ProgressRequest (scoped cleanly to remaining delta)
    // and enough goals in that career to unlock milestone 1..=4 or 101
    let sp_before = ctx.db.get_character(char_id).unwrap().free_skill_points;
    let mut char_id_msg = Vec::new();
    encode_varint_field(1, char_id as u64, &mut char_id_msg);
    let mut prog_req = uuid_req(travel_goal.uuid);
    encode_varint_field(2, travel_goal.target.max(1), &mut prog_req);
    encode_bytes_field(3, &char_id_msg, &mut prog_req);
    assert_eq!(
        quasar_rpc(
            &ctx,
            &mut gm,
            "eve_public.career.goal.api.admin.ProgressRequest",
            prog_req,
        )
        .await
        .status_code,
        STATUS_OK
    );

    // Claim completed career goal -> SP/ISK granted, double-claim rejected STATUS_FORBIDDEN
    let claim_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.career.goal.api.ClaimRewardRequest",
        uuid_req(travel_goal.uuid),
    )
    .await;
    assert_eq!(claim_rsp.status_code, STATUS_OK);
    assert_eq!(
        quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.career.goal.api.ClaimRewardRequest",
            uuid_req(travel_goal.uuid),
        )
        .await
        .status_code,
        STATUS_FORBIDDEN
    );

    // Check milestone threshold rejection before meeting 100+ points
    let mut cm_uuid = [0u8; 16];
    cm_uuid[0] = 0x43;
    cm_uuid[1] = 0x4D;
    cm_uuid[12..16].copy_from_slice(&101_u32.to_be_bytes());
    let early_ms = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.career.goal.api.ClaimRewardRequest",
        uuid_req(cm_uuid),
    )
    .await;
    if early_ms.status_code == STATUS_FORBIDDEN {
        // Complete additional career goals via GM admin ProgressRequest until overall CP >= milestone threshold
        let mut pts = ctx
            .quasar_gateway
            .progression
            .career_goals
            .career_points_for_character(char_id as u32, None);
        for d in defs.iter().filter(|d| d.uuid != travel_goal.uuid) {
            if pts >= 1_000 {
                break;
            }
            let mut req = uuid_req(d.uuid);
            encode_varint_field(2, d.target.max(1), &mut req);
            encode_bytes_field(3, &char_id_msg, &mut req);
            let _ = quasar_rpc(
                &ctx,
                &mut gm,
                "eve_public.career.goal.api.admin.ProgressRequest",
                req,
            )
            .await;
            pts += d.career_points;
        }
        let ms_ok = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.career.goal.api.ClaimRewardRequest",
            uuid_req(cm_uuid),
        )
        .await;
        assert_eq!(ms_ok.status_code, STATUS_OK);
    }

    let sp_after = ctx.db.get_character(char_id).unwrap().free_skill_points;
    assert!(sp_after >= sp_before, "Claiming AIR goal/milestone grants SP");

    // 4. SQLite reload preserves career goal progress, claims, and milestone lock
    let ctx2 = reload_ctx_from_sqlite(&ctx);
    let (mut sess2, _) =
        login_existing_player(&ctx2, 60_073, "acp_pilot", "acp_pilot-secret", char_id).await;
    assert_eq!(ctx2.db.get_character(char_id).unwrap().free_skill_points, sp_after);
    assert_eq!(
        quasar_rpc(
            &ctx2,
            &mut sess2,
            "eve_public.career.goal.api.ClaimRewardRequest",
            uuid_req(travel_goal.uuid),
        )
        .await
        .status_code,
        STATUS_FORBIDDEN
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quasar_economy_industry_h2c_and_persistence_end_to_end() {
    // parity: plan:Q-6
    eve_testkit::attest(&["plan:Q-6"]);

    let ctx = Arc::new(fresh_ctx_full_market());
    let (mut sess, char_id, _) =
        setup_pilot_in_full_market(&ctx, 60_081, "q6_econ", "Q6 Industrialist").await;
    assert_eq!(sess.role & service_roles::ROLE_GM_MASK, 0);

    // 1. Mutate economy & industry via REAL non-GM MachoNet RPCs (zero with_mut_snapshot)
    let trit_item_id = buy_item_valid_tick(&ctx, &mut sess, char_id, JITA_STATION, 34, 250).await;
    let sell_res = rpc_ok(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::Dict(vec![
                (PyRep::String("itemID".into()), PyRep::Integer(trit_item_id)),
                (PyRep::String("typeID".into()), PyRep::Integer(34)),
                (PyRep::String("price".into()), PyRep::Real(6.5)),
                (PyRep::String("quantity".into()), PyRep::Integer(250)),
                (PyRep::String("stationID".into()), PyRep::Integer(JITA_STATION)),
                (PyRep::String("duration".into()), PyRep::Integer(90)),
            ])]),
            PyRep::Bool(false),
        ],
    )
    .await;
    let placed_order_id = sell_res
        .dict_get("orderID")
        .and_then(PyRep::as_i64)
        .expect("market orderID");
    assert!(placed_order_id > 0);

    let (bp_tid, bp_mats) = pick_small_seeded_bpo(&ctx, JITA_STATION);
    let _bpo = buy_item_valid_tick(&ctx, &mut sess, char_id, JITA_STATION, bp_tid, 1).await;
    for m in bp_mats {
        let _ = buy_item_valid_tick(&ctx, &mut sess, char_id, JITA_STATION, m.type_id, m.quantity as i64).await;
    }
    let job_res = rpc_ok(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(i64::from(bp_tid)), PyRep::Integer(1), PyRep::Integer(1)],
    )
    .await;
    let installed_job_id = job_res.dict_get("jobID").and_then(PyRep::as_i64).expect("jobID");
    assert!(installed_job_id > 0);

    // 2. Start live HTTP/2 h2c Quasar gateway listener and issue bearer JWT
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral h2c port");
    let addr = listener.local_addr().unwrap();
    let srv_ctx = Arc::clone(&ctx);
    let srv_task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let c = Arc::clone(&srv_ctx);
            tokio::spawn(async move {
                let _ = eve_server::quasar_grpc::handle_quasar_grpc_connection(stream, c).await;
            });
        }
    });
    tokio::task::yield_now().await;

    let uid = sess.user_id.expect("uid");
    let now = ctx.now_unix();
    let token = ctx
        .db
        .issue_gateway_dev_token(uid, Some(char_id), "eve-test", now + 3_600, now)
        .expect("issue gateway JWT");

    let make_pub_req = |type_name: &str, value: Vec<u8>| EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: now as i64,
            nanos: 0,
        }),
        correlation_uuid: b"q6-h2c-corr".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"q6-h2c-app".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };

    // Unauthenticated h2c request must reject with gRPC status 16 (UNAUTHENTICATED)
    let (unauth_grpc, _) = send_h2c_quasar_request(
        addr,
        None,
        &make_pub_req("eve.market.price.GetForAllTypesRequest", Vec::new()),
    )
    .await;
    assert_eq!(unauth_grpc.as_deref(), Some("16"));

    // Authenticated h2c queries across Q-6 packages
    let jita_planet_iv = ctx
        .sde
        .celestials_in_system(JITA_SYS)
        .into_iter()
        .find(|c| c.group_id == 7)
        .map(|c| c.celestial_id)
        .unwrap_or(40_009_122);
    let mut ord_q = id_payload(1, 10_000_002);
    ord_q.extend_from_slice(&id_payload(2, JITA_STATION));
    ord_q.extend_from_slice(&id_payload(3, 34));
    for (msg_type, payload, require_non_empty) in [
        ("eve.market.orders.GetOrdersRequest", ord_q, true),
        ("eve.market.orders.GetOpenForCharacterRequest", id_payload(1, char_id), true),
        ("eve.market.price.GetForAllTypesRequest", Vec::new(), true),
        ("eve.inventory.genericitemtype.GetRequest", id_payload(1, 34), true),
        ("eve.inventory.genericname.GetNameByIDRequest", id_payload(1, 34), true),
        ("eve.asset.GetAssetsItemsRequest", id_payload(1, char_id), true),
        (
            "eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest",
            id_payload(1, char_id),
            false,
        ),
        (
            "eve.accounting.character.journal.GetCharacterEntriesRequest",
            id_payload(1, char_id),
            true,
        ),
        ("eve.character.industry.job.GetRequest", id_payload(1, char_id), true),
        (
            "eve.character.industry.blueprints.GetRequest",
            id_payload(1, char_id),
            false,
        ),
        ("eve.industry.facility.GetRequest", Vec::new(), true),
        ("eve.industry.system.GetCostIndicesRequest", Vec::new(), true),
        ("eve.planet.GetRequest", id_payload(1, jita_planet_iv), true),
        (
            "eve.planet.GetPlanetsInSolarSystemRequest",
            id_payload(1, i64::from(JITA_SYS)),
            true,
        ),
        (
            "eve.planetinteraction.collector.GetRequest",
            id_payload(1, char_id),
            false,
        ),
        ("eve.moon.GetMoonsOfPlanetRequest", id_payload(1, jita_planet_iv), true),
        (
            "eve.asteroidbelt.GetBeltsOfPlanetRequest",
            id_payload(1, jita_planet_iv),
            true,
        ),
        (
            "eve.inventory.assetholding.operation.api.HoldItemsRequest",
            id_payload(1, 42),
            true,
        ),
        (
            "eve.contract.item_exchange.GetRequest",
            id_payload(1, char_id),
            false,
        ),
    ] {
        let (grpc_st, rsp_opt) =
            send_h2c_quasar_request(addr, Some(&token), &make_pub_req(msg_type, payload)).await;
        assert_eq!(grpc_st.as_deref(), Some("0"), "{msg_type} gRPC status");
        let rsp = rsp_opt.expect("EvePublicResponse over h2c");
        assert_eq!(rsp.status_code, STATUS_OK, "{msg_type} Quasar status");
        let any = rsp.payload.as_ref().expect("GoogleAny payload");
        assert!(
            any.type_url.ends_with("Response"),
            "{msg_type} unexpected response type_url: {}",
            any.type_url
        );
        if require_non_empty {
            assert!(
                !any.value.is_empty(),
                "{msg_type} must return non-empty protobuf payload"
            );
        }
    }

    // Verify loyaltypoints assetholding DepositRequest & WithdrawRequest over h2c (non-GM forbidden, GM succeeds)
    let mut lp_op_id = Vec::new();
    encode_bytes_field(1, &[0x61u8; 16], &mut lp_op_id);
    let mut lp_wallet = id_payload(1, char_id);
    lp_wallet.extend_from_slice(&id_payload(3, 1_000_035));
    let mut lp_curr = Vec::new();
    encode_varint_field(1, 500, &mut lp_curr);
    let mut lp_attr = Vec::new();
    encode_bytes_field(1, &lp_wallet, &mut lp_attr);
    encode_bytes_field(2, &lp_curr, &mut lp_attr);
    let mut lp_dep = Vec::new();
    encode_bytes_field(1, &lp_op_id, &mut lp_dep);
    encode_bytes_field(2, &lp_attr, &mut lp_dep);
    let (_, non_gm_lp) = send_h2c_quasar_request(
        addr,
        Some(&token),
        &make_pub_req(
            "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
            lp_dep.clone(),
        ),
    )
    .await;
    assert_eq!(non_gm_lp.unwrap().status_code, STATUS_FORBIDDEN);
    if !ctx.db.with_snapshot(|s| s.accounts.contains_key("gm_remediate")) {
        ctx.db
            .provision_gm_account("gm_remediate", "gm-pass", 90_000_099, "GM Remediate");
    }
    let gm_pkce = ctx
        .db
        .authorize_pkce(
            "gm_remediate",
            "gm-pass",
            90_000_099,
            "eve-test",
            "https://localhost/callback",
            Vec::new(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "S256",
            now,
        )
        .expect("authorize gm PKCE");
    let gm_token = ctx
        .db
        .exchange_pkce_code(
            &gm_pkce,
            "eve-test",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now,
        )
        .expect("issue gm full SSO JWT")
        .access_token;
    let (_, gm_dep) = send_h2c_quasar_request(
        addr,
        Some(&gm_token),
        &make_pub_req(
            "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
            lp_dep,
        ),
    )
    .await;
    assert_eq!(gm_dep.unwrap().status_code, STATUS_OK);
    let mut lp_wd_id = Vec::new();
    encode_bytes_field(1, &[0x62u8; 16], &mut lp_wd_id);
    let mut lp_wd = Vec::new();
    encode_bytes_field(1, &lp_wd_id, &mut lp_wd);
    encode_bytes_field(2, &lp_attr, &mut lp_wd);
    let (_, gm_wd) = send_h2c_quasar_request(
        addr,
        Some(&gm_token),
        &make_pub_req(
            "eve.loyaltypoints.assetholding.operation.api.WithdrawRequest",
            lp_wd,
        ),
    )
    .await;
    assert_eq!(gm_wd.unwrap().status_code, STATUS_OK);

    // Unknown item type over h2c returns STATUS_NOT_FOUND or STATUS_BAD_REQUEST
    let (_, bad_item_rsp) = send_h2c_quasar_request(
        addr,
        Some(&token),
        &make_pub_req("eve.inventory.genericitemtype.GetRequest", id_payload(1, 999_999_999)),
    )
    .await;
    let bad_st = bad_item_rsp.unwrap().status_code;
    assert!(bad_st == STATUS_NOT_FOUND || bad_st == STATUS_BAD_REQUEST);
    srv_task.abort();

    // 3. Reload SQLite and verify market order & industry job persist over fresh Quasar gateway!
    let ctx2 = Arc::new(reload_ctx_from_sqlite(&ctx));
    let (mut sess2, _) =
        login_existing_player(&ctx2, 60_082, "q6_econ", "q6_econ-secret", char_id).await;
    let jobs_reload = quasar_rpc(
        &ctx2,
        &mut sess2,
        "eve.character.industry.job.GetRequest",
        id_payload(1, char_id),
    )
    .await;
    assert_eq!(jobs_reload.status_code, STATUS_OK);
    let job_bytes = jobs_reload.payload.unwrap().value;
    assert!(!job_bytes.is_empty());

    let orders_reload = quasar_rpc(
        &ctx2,
        &mut sess2,
        "eve.market.orders.GetOpenForCharacterRequest",
        id_payload(1, char_id),
    )
    .await;
    assert_eq!(orders_reload.status_code, STATUS_OK);
    assert!(!orders_reload.payload.unwrap().value.is_empty());
    assert!(ctx2.db.with_snapshot(|s| s.market_orders.contains_key(&placed_order_id)));
    assert!(ctx2.db.with_snapshot(|s| s.industry_jobs.contains_key(&installed_job_id)));
}
