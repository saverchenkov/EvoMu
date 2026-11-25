//! Unit and live-socket tests for Track 1C bounded `SystemActor` asks, unified
//! `on_actor_ask_timeout_or_failure` handling, and next-tick `SetState` recovery (`#1170`).

#![forbid(unsafe_code)]

use super::super::macho_listener_extra_tests::decode_wrapped_user_error;
use super::*;
use crate::outbound_sequencer::{DestinyStream, OutFrame, SessionWireState};
use crate::sequencer_invariants::parse_destiny_entry;
use crate::system_actor::SystemHandle;
use eve_machonet::{
    placebo_crypto_hash, CryptoChallengePacket, CryptoHandshakeAck, CryptoHandshakeResult,
    CryptoRequestPacket, CryptoServerHandshake,
};
use std::time::Duration;
use tokio::net::TcpListener;

struct PausedActorGuard {
    release_tx: Option<std::sync::mpsc::Sender<()>>,
    join_handle: Option<std::thread::JoinHandle<()>>,
}

impl PausedActorGuard {
    fn pause(sys_handle: &SystemHandle) -> Self {
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let sys_pause = sys_handle.clone();
        let join_handle = std::thread::spawn(move || {
            let _bp = sys_pause.ballpark.write().unwrap();
            held_tx.send(()).unwrap();
            let _ = release_rx.recv();
        });
        held_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("PausedActorGuard acquire ballpark write lock");
        Self {
            release_tx: Some(release_tx),
            join_handle: Some(join_handle),
        }
    }

    fn resume(mut self) {
        if let Some(tx) = self.release_tx.take() {
            let _ = tx.send(());
        }
        if let Some(jh) = self.join_handle.take() {
            jh.join().unwrap();
        }
    }
}

impl Drop for PausedActorGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.release_tx.take() {
            let _ = tx.send(());
        }
        if let Some(jh) = self.join_handle.take() {
            let _ = jh.join();
        }
    }
}

fn spawn_test_ship_ball(sys_handle: &SystemHandle, ship_id: i64, x_offset: f64) -> eve_destiny::Ball {
    let _ = sys_handle;
    eve_destiny::Ball {
        id: ship_id,
        type_id: 587,
        position: eve_destiny::DVec3::new(100_000.0 + x_offset, 0.0, 0.0),
        velocity: eve_destiny::DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass: 1_100_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    }
}

async fn setup_bootstrapped_space_session(
    sid: u64,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
) -> (ServerContext, SystemHandle, SessionState) {
    let ctx = ServerContext::new();
    let user_id = 1_u64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = user_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.owner_id = char_id;
            sh.location_id = i64::from(sys_id);
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball(spawn_test_ship_ball(&sys_handle, ship_id, 0.0))
        .await
        .expect("spawn ship ball");
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), true, ctx.now_unix())
        .expect("acquire online lock");

    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess.live_notify_listener = true;

    let kwargs = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "beyonce", None, "GetFormations", &[], &kwargs, None)
        .await
        .expect("warm initial GetFormations ballpark bootstrap");
    let _ = sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_id);
    (ctx, sys_handle, sess)
}

async fn emit_initial_live_set_state(
    ctx: &ServerContext,
    sys_handle: &SystemHandle,
    sys_rx: &mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>,
    wire_state: &mut SessionWireState,
    server_wr: &mut tokio::io::DuplexStream,
    client_rd: &mut tokio::io::DuplexStream,
    sess: &SessionState,
    sys_id: u32,
    observer_id: i64,
    shared_structure_observer: bool,
) -> u32 {
    let ep = wire_state.enter_space(Some(sys_id));
    let initial = if shared_structure_observer {
        resync_structure_set_state_with_sys_rx(
            ctx,
            sys_handle,
            sys_rx,
            sys_id,
            observer_id,
            &[observer_id],
            wire_state,
        )
        .await
        .expect("initial structure SetState")
    } else {
        resync_set_state_with_sys_rx(
            ctx,
            sys_handle,
            sys_rx,
            sys_id,
            observer_id,
            &[observer_id],
            wire_state,
        )
        .await
        .expect("initial ship SetState")
    };
    let init_batch = build_destiny_batch(
        ctx,
        ep,
        initial.tick,
        &initial.events,
        initial.updates_pyrep,
        &[observer_id],
        SelfDestinyWireContext::NonDock,
        crate::outbound_scene_scope::space_scope(Some(sys_id), Some(wire_state.scene_epoch)),
    )
    .expect("initial SetState batch");
    emit_tick_batch_with_observability(
        ctx,
        wire_state,
        server_wr,
        wire_state.client_id,
        sess,
        init_batch,
        "initial_test_set_state",
    )
    .await
    .expect("emit initial SetState");
    let _ = read_framed_pyrep(client_rd).await.unwrap().unwrap();
    assert!(matches!(
        wire_state.destiny,
        DestinyStream::Live { epoch, .. } if epoch == ep
    ));
    assert!(!wire_state.needs_awaiting_set_state_resync());
    ep
}

fn packet_has_destiny_action(pkt: &MachoPacket, action_name: &str) -> bool {
    fn contains_action(rep: &PyRep, action_name: &str) -> bool {
        if matches!(parse_destiny_entry(rep), Some((_, act, _)) if act == action_name) {
            return true;
        }
        match rep {
            PyRep::Tuple(items) | PyRep::List(items) => {
                items.iter().any(|it| contains_action(it, action_name))
            }
            PyRep::SubStream(inner) | PyRep::SubStruct(inner) => {
                contains_action(inner, action_name)
            }
            _ => false,
        }
    }
    pkt.packet_type == MachoPacketType::Notification
        && matches!(
            &pkt.destination,
            MachoAddress::Broadcast { broadcast_id, .. } if broadcast_id == "DoDestinyUpdate"
        )
        && contains_action(&pkt.payload, action_name)
}

async fn assert_next_tick_recovers_set_state_without_client_rpc(
    ctx: &ServerContext,
    sys_handle: &SystemHandle,
    sys_rx: &mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>,
    wire_state: &mut SessionWireState,
    server_wr: &mut tokio::io::DuplexStream,
    client_rd: &mut tokio::io::DuplexStream,
    sess: &SessionState,
    observer_id: i64,
    ep: u32,
    pause_guard: PausedActorGuard,
) {
    assert_eq!(wire_state.destiny, DestinyStream::AwaitingSetState { epoch: ep });
    assert!(wire_state.needs_awaiting_set_state_resync());

    pause_guard.resume();

    // Advance tick without the client sending any RPC: next tick must recover SetState.
    sys_handle.tick().await.expect("step system tick");
    let tick_msg = tokio::time::timeout(Duration::from_secs(2), sys_rx.recv())
        .await
        .expect("receive post-recovery tick")
        .expect("tick broadcast ok");
    handle_live_space_tick_broadcast(
        ctx,
        Some(sys_handle),
        server_wr,
        wire_state,
        Some(sys_rx),
        wire_state.client_id,
        sess,
        observer_id,
        &tick_msg,
    )
    .await
    .expect("recover AwaitingSetState on Ok(tick_msg)");

    assert!(
        matches!(wire_state.destiny, DestinyStream::Live { epoch, .. } if epoch == ep),
        "expected DestinyStream::Live after Ok(tick_msg) SetState resync, got {:?}",
        wire_state.destiny
    );
    assert!(!wire_state.needs_awaiting_set_state_resync());

    let recovery_rep = tokio::time::timeout(Duration::from_secs(1), read_framed_pyrep(client_rd))
        .await
        .expect("read SetState recovery wire frame")
        .expect("read ok")
        .expect("non-eof frame");
    let pkt = MachoPacket::from_pyrep(&recovery_rep).expect("MachoPacket");
    assert!(
        packet_has_destiny_action(&pkt, "SetState"),
        "expected DoDestinyUpdate(SetState) on wire after AwaitingSetState recovery, got {pkt:?}"
    );

    // Subsequent tick must continue as normal Live stream without looping SetState.
    sys_handle.tick().await.expect("step system tick");
    let delta_tick = tokio::time::timeout(Duration::from_secs(2), sys_rx.recv())
        .await
        .expect("receive subsequent tick")
        .expect("tick ok");
    handle_live_space_tick_broadcast(
        ctx,
        Some(sys_handle),
        server_wr,
        wire_state,
        Some(sys_rx),
        wire_state.client_id,
        sess,
        observer_id,
        &delta_tick,
    )
    .await
    .expect("emit normal delta tick after SetState recovery");
    assert!(matches!(
        wire_state.destiny,
        DestinyStream::Live { epoch, .. } if epoch == ep
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_rpc_bootstrap_ask_against_paused_actor_returns_custom_notify_and_recovers_on_next_tick() {
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sid = 91_499_u64;
    let client_id = sid * 10;
    let (ctx, sys_handle, mut sess) =
        setup_bootstrapped_space_session(sid, sys_id, char_id, ship_id).await;

    let mut sys_rx = sys_handle.subscribe();
    let mut subscribed_sys_id = Some(sys_id);
    let mut wire_state = SessionWireState::new(sid, client_id);
    let (mut client_rd, mut server_wr) = tokio::io::duplex(64 * 1024);
    let ep = emit_initial_live_set_state(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        sys_id,
        ship_id,
        false,
    )
    .await;

    sess.notifications
        .push(("OnSessionChanged".to_string(), vec![PyRep::None]));
    let pause_guard = PausedActorGuard::pause(&sys_handle);

    let req_source = MachoAddress::Client {
        client_id,
        call_id: 77,
        service: None,
    };
    let req_dest = MachoAddress::Node {
        node_id: eve_machonet::MACHO_PROXY_NODE_ID,
        service: Some("beyonce".to_string()),
        call_id: 0,
    };
    let mut deferred_ship_id = None;
    let mut wire_refs = CallReqWireRefs {
        client_id,
        sys_rx: &mut sys_rx,
        subscribed_sys_id: &mut subscribed_sys_id,
        wire_state: &mut wire_state,
        deferred_ship_id: &mut deferred_ship_id,
    };
    let kwargs = HashMap::new();
    let ask_start = std::time::Instant::now();
    let outcome = execute_call_req_wire_with_ask_timeout(
        &ctx,
        &mut sess,
        &mut wire_refs,
        &req_source,
        &req_dest,
        "beyonce",
        None,
        "GetFormations",
        &[],
        &kwargs,
        Duration::from_millis(120),
    )
    .await;
    assert!(
        ask_start.elapsed() < Duration::from_millis(750),
        "bootstrap post-RPC ask against paused actor must fail within bounded deadline"
    );
    let err = outcome
        .rpc_result
        .expect_err("paused actor Destiny-bootstrap ask must fail instead of hanging");
    assert_eq!(err.code, "ServiceTemporarilyUnavailable");
    assert!(
        matches!(
            err.to_client_error("beyonce", "GetFormations"),
            eve_proto::ClientError::CustomNotify(ref msg)
                if msg.contains("drain_observer_updates_with_seq") && msg.contains("ask timed out")
        ),
        "expected structured CustomNotify timeout error, got {err:?}"
    );
    let err_body = outcome
        .error_wire_body
        .expect("timeout must serialize structured ErrorResponse wire payload");
    let (user_key, user_dict) = decode_wrapped_user_error(err_body);
    assert_eq!(user_key, "CustomNotify");
    assert!(
        user_dict.iter().any(|(k, v)| {
            k == "notify"
                && v.as_str()
                    .is_some_and(|s| s.contains("drain_observer_updates_with_seq"))
        }),
        "expected notify payload referencing timed-out actor ask, got {user_dict:?}"
    );
    assert!(outcome
        .out_frames
        .iter()
        .any(|f| matches!(f, OutFrame::CallRsp { .. })));
    assert!(
        !outcome
            .out_frames
            .iter()
            .any(|f| matches!(f, OutFrame::SessionChange { .. }))
            && outcome
                .notifications
                .iter()
                .all(|(ev, _)| ev != "OnSessionChanged"),
        "ErrorResponse on Destiny-bootstrap timeout must suppress contradictory SessionChange frames"
    );

    assert_next_tick_recovers_set_state_without_client_rpc(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        ship_id,
        ep,
        pause_guard,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_rpc_non_bootstrap_ask_timeout_preserves_ok_and_recovers_set_state_on_next_tick() {
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sid = 91_502_u64;
    let client_id = sid * 10;
    let (ctx, sys_handle, mut sess) =
        setup_bootstrapped_space_session(sid, sys_id, char_id, ship_id).await;

    let mut sys_rx = sys_handle.subscribe();
    let mut subscribed_sys_id = Some(sys_id);
    let mut wire_state = SessionWireState::new(sid, client_id);
    let (mut client_rd, mut server_wr) = tokio::io::duplex(64 * 1024);
    let ep = emit_initial_live_set_state(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        sys_id,
        ship_id,
        false,
    )
    .await;

    let pause_guard = PausedActorGuard::pause(&sys_handle);
    let req_source = MachoAddress::Client {
        client_id,
        call_id: 88,
        service: None,
    };
    let req_dest_acct = MachoAddress::Node {
        node_id: eve_machonet::MACHO_PROXY_NODE_ID,
        service: Some("account".to_string()),
        call_id: 0,
    };
    let mut deferred_ship_id = None;
    let mut wire_refs = CallReqWireRefs {
        client_id,
        sys_rx: &mut sys_rx,
        subscribed_sys_id: &mut subscribed_sys_id,
        wire_state: &mut wire_state,
        deferred_ship_id: &mut deferred_ship_id,
    };
    let kwargs = HashMap::new();
    let ask_start = std::time::Instant::now();
    let acct_outcome = execute_call_req_wire_with_ask_timeout(
        &ctx,
        &mut sess,
        &mut wire_refs,
        &req_source,
        &req_dest_acct,
        "account",
        None,
        "GetCashBalance",
        &[],
        &kwargs,
        Duration::from_millis(100),
    )
    .await;
    assert!(ask_start.elapsed() < Duration::from_millis(600));
    assert!(
        acct_outcome.rpc_result.is_ok(),
        "A3: non-bootstrap post-RPC observer timeout must preserve committed Ok reply: {:?}",
        acct_outcome.rpc_result
    );

    assert_next_tick_recovers_set_state_without_client_rpc(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        ship_id,
        ep,
        pause_guard,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_flush_ask_timeout_marks_awaiting_set_state_and_recovers_on_next_tick() {
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let struct_id = 1_040_001_170_301_i64;
    let sid = 91_503_u64;
    let client_id = sid * 10;

    let (ctx, sys_handle, mut sess) =
        setup_bootstrapped_space_session(sid, sys_id, char_id, ship_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
        }
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                solar_system_id: sys_id,
                owner_corp_id: 1_000_001,
                name: "Test Structure".into(),
                position: [250_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    sys_handle
        .spawn_ball(spawn_test_ship_ball(&sys_handle, struct_id, 50_000.0))
        .await
        .expect("spawn structure ball");
    sess.structure_id = Some(struct_id);
    ctx.mark_space_ballpark_bootstrapped(sid, char_id, sys_id);

    let mut sys_rx = sys_handle.subscribe();
    let mut subscribed_sys_id = Some(sys_id);
    let mut wire_state = SessionWireState::new(sid, client_id);
    let (mut client_rd, mut server_wr) = tokio::io::duplex(64 * 1024);
    let ep = emit_initial_live_set_state(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        sys_id,
        struct_id,
        true,
    )
    .await;

    // 1) End-to-end RPC on live shared-structure View Outside skips observer drain and hits
    //    `flush_peer_observer_updates` against the paused actor.
    let pause_guard = PausedActorGuard::pause(&sys_handle);
    let req_source = MachoAddress::Client {
        client_id,
        call_id: 99,
        service: None,
    };
    let req_dest_acct = MachoAddress::Node {
        node_id: eve_machonet::MACHO_PROXY_NODE_ID,
        service: Some("account".to_string()),
        call_id: 0,
    };
    let mut deferred_ship_id = None;
    let mut wire_refs = CallReqWireRefs {
        client_id,
        sys_rx: &mut sys_rx,
        subscribed_sys_id: &mut subscribed_sys_id,
        wire_state: &mut wire_state,
        deferred_ship_id: &mut deferred_ship_id,
    };
    let kwargs = HashMap::new();
    let ask_start = std::time::Instant::now();
    let acct_outcome = execute_call_req_wire_with_ask_timeout(
        &ctx,
        &mut sess,
        &mut wire_refs,
        &req_source,
        &req_dest_acct,
        "account",
        None,
        "GetCashBalance",
        &[],
        &kwargs,
        Duration::from_millis(80),
    )
    .await;
    assert!(ask_start.elapsed() < Duration::from_millis(500));
    assert!(acct_outcome.rpc_result.is_ok());

    assert_next_tick_recovers_set_state_without_client_rpc(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        struct_id,
        ep,
        pause_guard,
    )
    .await;

    // 2) Direct helper check while paused: `best_effort_flush_peer_observer_updates` returns false
    //    and leaves `wire_state` in `AwaitingSetState` (`A1`).
    let pause_guard_direct = PausedActorGuard::pause(&sys_handle);
    let flush_ok = best_effort_flush_peer_observer_updates(
        &mut wire_state,
        &sys_handle,
        Some(struct_id),
        Duration::from_millis(60),
        "direct_test",
    )
    .await;
    assert!(!flush_ok);
    assert_next_tick_recovers_set_state_without_client_rpc(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        struct_id,
        ep,
        pause_guard_direct,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_in_space_ship_swap_destiny_timeout_marks_awaiting_set_state_and_recovers() {
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let old_ship_id = 100_000_001_i64;
    let new_ship_id = 100_000_099_i64;
    let sid = 91_501_u64;
    let client_id = sid * 10;

    let (ctx, sys_handle, sess) =
        setup_bootstrapped_space_session(sid, sys_id, char_id, old_ship_id).await;
    sys_handle
        .spawn_ball(spawn_test_ship_ball(&sys_handle, new_ship_id, 500.0))
        .await
        .expect("spawn new ship ball");

    let mut sys_rx = sys_handle.subscribe();
    let mut wire_state = SessionWireState::new(sid, client_id);
    let (mut client_rd, mut server_wr) = tokio::io::duplex(64 * 1024);
    let ep = emit_initial_live_set_state(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        sys_id,
        old_ship_id,
        false,
    )
    .await;

    let pause_guard = PausedActorGuard::pause(&sys_handle);
    let start = std::time::Instant::now();
    let sync = sync_in_space_ship_swap_destiny_bounded(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        sid,
        sys_id,
        new_ship_id,
        Some(old_ship_id),
        true,
        Duration::from_millis(80),
    )
    .await;
    assert!(start.elapsed() < Duration::from_millis(500));
    assert!(sync.is_none());

    assert_next_tick_recovers_set_state_without_client_rpc(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        new_ship_id,
        ep,
        pause_guard,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resync_set_state_ask_sites_ship_and_structure_timeout_and_recover_on_next_tick() {
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sid = 91_504_u64;
    let client_id = sid * 10;

    let (ctx, sys_handle, mut sess) =
        setup_bootstrapped_space_session(sid, sys_id, char_id, ship_id).await;
    let mut sys_rx = sys_handle.subscribe();
    let mut wire_state = SessionWireState::new(sid, client_id);
    let (mut client_rd, mut server_wr) = tokio::io::duplex(64 * 1024);
    let ep = emit_initial_live_set_state(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        sys_id,
        ship_id,
        false,
    )
    .await;

    // 1) Ship resync (`push_set_state` / `drain_observer_updates_with_seq`) against paused actor.
    let pause_ship = PausedActorGuard::pause(&sys_handle);
    let ship_err = resync_set_state_with_sys_rx_with_mode_bounded(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        sys_id,
        ship_id,
        &[ship_id],
        &mut wire_state,
        false,
        Duration::from_millis(80),
    )
    .await
    .err()
    .expect("ship resync against paused actor must fail with bounded error");
    assert_eq!(ship_err.code, "ServiceTemporarilyUnavailable");
    assert_next_tick_recovers_set_state_without_client_rpc(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        ship_id,
        ep,
        pause_ship,
    )
    .await;

    // 2) Shared structure resync (`get_set_state_with_seq`) against paused actor.
    sess.structure_id = Some(ship_id);
    let pause_struct = PausedActorGuard::pause(&sys_handle);
    let struct_err = resync_set_state_with_sys_rx_with_mode_bounded(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        sys_id,
        ship_id,
        &[ship_id],
        &mut wire_state,
        true,
        Duration::from_millis(80),
    )
    .await
    .err()
    .expect("structure resync against paused actor must fail with bounded error");
    assert_eq!(struct_err.code, "ServiceTemporarilyUnavailable");
    assert_next_tick_recovers_set_state_without_client_rpc(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        ship_id,
        ep,
        pause_struct,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_dock_and_docking_transition_asks_against_paused_actor_stay_bounded_and_recover() {
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sid = 91_505_u64;
    let client_id = sid * 10;

    let (ctx, sys_handle, mut sess) =
        setup_bootstrapped_space_session(sid, sys_id, char_id, ship_id).await;
    let mut sys_rx = sys_handle.subscribe();
    let mut subscribed_sys_id = Some(sys_id);
    let mut wire_state = SessionWireState::new(sid, client_id);
    let (mut client_rd, mut server_wr) = tokio::io::duplex(64 * 1024);
    let ep = emit_initial_live_set_state(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        sys_id,
        ship_id,
        false,
    )
    .await;

    // 1) `run_pending_dock_settle_destiny_sync` against paused actor stays bounded and sets
    //    `AwaitingSetState`, then recovers `SetState` on next tick after resume.
    let plan = crate::services::space_warp_jump::DockWarpSettlePlan {
        sys_id,
        ship_id,
        stopped_now: true,
        settle_deadline: tokio::time::Instant::now() + Duration::from_secs(1),
    };
    let pause_dock = PausedActorGuard::pause(&sys_handle);
    let dock_start = std::time::Instant::now();
    run_pending_dock_settle_destiny_sync(
        &ctx,
        &sys_handle,
        &mut server_wr,
        &mut wire_state,
        Some(&mut sys_rx),
        client_id,
        sid,
        &sess,
        &plan,
        Duration::from_millis(200),
    )
    .await
    .expect("pending dock settle timeout must not break connection");
    // NB-1: when `drain_observer_updates_with_seq` times out, peer flush is skipped so total
    // latency stays 1x (~200ms) rather than 2x (~400ms).
    let dock_elapsed = dock_start.elapsed();
    assert!(
        dock_elapsed >= Duration::from_millis(180) && dock_elapsed < Duration::from_millis(360),
        "expected single bounded ask deadline (~200ms), got {dock_elapsed:?}"
    );

    assert_next_tick_recovers_set_state_without_client_rpc(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        ship_id,
        ep,
        pause_dock,
    )
    .await;

    // 2) `death_respawn_docking`, `pre_dock`, and `docking_transition` peer-flush asks against
    //    a paused actor stay bounded, route through `on_actor_ask_timeout_or_failure`, and
    //    mark `AwaitingSetState` (`on_broadcast_lagged()`), after which unpaused `stationSvc.Dock`
    //    still succeeds and detaches cleanly.
    let pause_death = PausedActorGuard::pause(&sys_handle);
    let death_start = std::time::Instant::now();
    let death_err = run_death_respawn_docking_ask_bounded(
        &mut wire_state,
        &sys_handle,
        ship_id,
        Duration::from_millis(80),
    )
    .await
    .expect_err("paused death_respawn_docking must time out");
    assert!(death_start.elapsed() < Duration::from_millis(500));
    assert_eq!(death_err.code, "ServiceTemporarilyUnavailable");
    assert_eq!(wire_state.destiny, DestinyStream::AwaitingSetState { epoch: ep });
    assert_next_tick_recovers_set_state_without_client_rpc(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        ship_id,
        ep,
        pause_death,
    )
    .await;

    let pause_pre_dock = PausedActorGuard::pause(&sys_handle);
    let pre_dock_scope =
        crate::outbound_scene_scope::space_scope(Some(sys_id), Some(wire_state.scene_epoch));
    let pre_dock_start = std::time::Instant::now();
    let pre_dock_err = run_pre_dock_observer_ask_bounded(
        &ctx,
        &sys_handle,
        &mut wire_state,
        Some(&mut sys_rx),
        sys_id,
        ship_id,
        sid,
        pre_dock_scope,
        Duration::from_millis(80),
    )
    .await
    .expect_err("paused pre_dock observer ask must time out");
    assert!(pre_dock_start.elapsed() < Duration::from_millis(500));
    assert_eq!(pre_dock_err.code, "ServiceTemporarilyUnavailable");
    assert_eq!(wire_state.destiny, DestinyStream::AwaitingSetState { epoch: ep });

    let dock_peer_start = std::time::Instant::now();
    let dock_peer_ok = best_effort_flush_peer_observer_updates(
        &mut wire_state,
        &sys_handle,
        Some(ship_id),
        Duration::from_millis(80),
        "docking_transition",
    )
    .await;
    assert!(!dock_peer_ok);
    assert!(dock_peer_start.elapsed() < Duration::from_millis(500));
    assert_next_tick_recovers_set_state_without_client_rpc(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        ship_id,
        ep,
        pause_pre_dock,
    )
    .await;

    let station_pos = ctx.sde.stations[&60_003_760_i64].position;
    assert!(
        sys_handle.set_ball_position(ship_id, station_pos).await,
        "move ship inside station docking radius"
    );
    let req_source = MachoAddress::Client {
        client_id,
        call_id: 105,
        service: None,
    };
    let req_dest = MachoAddress::Node {
        node_id: eve_machonet::MACHO_PROXY_NODE_ID,
        service: Some("stationSvc".to_string()),
        call_id: 0,
    };
    let mut deferred_ship_id = None;
    let mut wire_refs = CallReqWireRefs {
        client_id,
        sys_rx: &mut sys_rx,
        subscribed_sys_id: &mut subscribed_sys_id,
        wire_state: &mut wire_state,
        deferred_ship_id: &mut deferred_ship_id,
    };
    let kwargs = HashMap::new();
    let dock_outcome = execute_call_req_wire_with_ask_timeout(
        &ctx,
        &mut sess,
        &mut wire_refs,
        &req_source,
        &req_dest,
        "stationSvc",
        None,
        "Dock",
        &[PyRep::Integer(60_003_760)],
        &kwargs,
        Duration::from_millis(80),
    )
    .await;
    assert!(
        dock_outcome.rpc_result.is_ok(),
        "unpaused stationSvc.Dock must succeed cleanly: {:?}",
        dock_outcome.rpc_result
    );
    assert_eq!(wire_state.destiny, DestinyStream::Detached);

    // 3) `notify_session_transition` (`drain_observer_updates`) against paused actor stays bounded
    //    and routes through `on_actor_ask_timeout_or_failure`.
    wire_state.enter_space(Some(sys_id));
    let pause_notify = PausedActorGuard::pause(&sys_handle);
    let notify_start = std::time::Instant::now();
    let notify_res = bounded_actor_ask(
        &mut wire_state,
        "drain_observer_updates",
        "notify_session_transition",
        Duration::from_millis(80),
        sys_handle.drain_observer_updates(ship_id),
    )
    .await;
    assert!(notify_start.elapsed() < Duration::from_millis(500));
    let notify_err = notify_res.expect_err("paused drain_observer_updates must time out");
    assert_eq!(notify_err.code, "ServiceTemporarilyUnavailable");
    assert!(matches!(wire_state.destiny, DestinyStream::AwaitingSetState { .. }));
    pause_notify.resume();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_macho_tcp_client_recovers_set_state_within_two_ticks_after_paused_actor_resumes() {
    async fn read_hs(stream: &mut TcpStream, stage: &str) -> PyRep {
        tokio::time::timeout(Duration::from_secs(5), read_framed_pyrep(stream))
            .await
            .unwrap_or_else(|_| panic!("handshake timeout at {stage}"))
            .unwrap_or_else(|e| panic!("handshake read error at {stage}: {e}"))
            .unwrap_or_else(|| panic!("handshake EOF at {stage}"))
    }

    let ctx = Arc::new(ServerContext::new());
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind tcp test listener");
    let addr = listener.local_addr().unwrap();
    let srv_ctx = Arc::clone(&ctx);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let c = Arc::clone(&srv_ctx);
            tokio::spawn(async move {
                let _ = handle_macho_client(stream, c).await;
            });
        }
    });

    // Connect and authenticate via 4-step placebo handshake.
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let _hello = read_hs(&mut stream, "hello").await;
    write_framed_pyrep(&mut stream, &VersionExchange::default().to_pyrep())
        .await
        .unwrap();
    write_framed_pyrep(&mut stream, &CryptoRequestPacket::placebo().to_pyrep())
        .await
        .unwrap();
    assert_eq!(read_hs(&mut stream, "ok_cc").await.as_str(), Some("OK CC"));
    let mut live_req = CryptoChallengePacket::new_password_login("demo", "demo-secret");
    live_req.user_password = None;
    write_framed_pyrep(&mut stream, &live_req.to_pyrep())
        .await
        .unwrap();
    let _ = read_hs(&mut stream, "live_req").await;
    write_framed_pyrep(
        &mut stream,
        &CryptoChallengePacket::new_password_login("demo", "demo-secret").to_pyrep(),
    )
    .await
    .unwrap();
    let srv_hs = CryptoServerHandshake::from_pyrep(&read_hs(&mut stream, "srv_hs").await).unwrap();
    write_framed_pyrep(
        &mut stream,
        &(CryptoHandshakeResult {
            challenge_response_hash: placebo_crypto_hash(&srv_hs.server_challenge),
            func_output: Vec::new(),
            func_result: PyRep::Integer(0),
        })
        .to_pyrep(),
    )
    .await
    .unwrap();
    let ack = CryptoHandshakeAck::from_pyrep(&read_hs(&mut stream, "ack").await).unwrap();
    let init_pkt = MachoPacket::from_pyrep(&read_hs(&mut stream, "init_pkt").await).unwrap();
    let MachoAddress::Client { client_id, .. } = init_pkt.destination else {
        panic!("expected Client destination on SessionInitialStateNotification");
    };
    let _sess_chg = MachoPacket::from_pyrep(&read_hs(&mut stream, "sess_chg").await).unwrap();

    async fn call_tcp(
        stream: &mut TcpStream,
        client_id: u64,
        user_id: u64,
        call_id: u64,
        service: &str,
        method: &str,
        args: Vec<PyRep>,
    ) -> (MachoPacket, Vec<MachoPacket>) {
        let req = MachoPacket {
            packet_type: MachoPacketType::CallReq,
            source: MachoAddress::Client {
                client_id,
                call_id,
                service: None,
            },
            destination: MachoAddress::Node {
                node_id: MACHO_PROXY_NODE_ID,
                service: Some(service.to_string()),
                call_id: 0,
            },
            user_id: Some(user_id),
            payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
                PyRep::Integer(0),
                PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                    PyRep::ascii_bytes(method),
                    PyRep::Tuple(args),
                    PyRep::Dict(Vec::new()),
                ]))),
            ])]),
            named_payload: HashMap::new(),
        };
        write_framed_pyrep(stream, &req.to_pyrep()).await.unwrap();
        let mut notifs = Vec::new();
        loop {
            let rep = tokio::time::timeout(Duration::from_secs(10), read_framed_pyrep(stream))
                .await
                .expect("CallRsp timeout")
                .expect("read ok")
                .expect("non-eof");
            let pkt = MachoPacket::from_pyrep(&rep).unwrap();
            if matches!(
                pkt.packet_type,
                MachoPacketType::CallRsp | MachoPacketType::ErrorResponse
            ) {
                return (pkt, notifs);
            }
            notifs.push(pkt);
        }
    }

    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    // Run the paused `SystemActor` on an isolated runtime so holding `ballpark.write()` inside
    // `PausedActorGuard` cannot starve the Tokio driver serving `handle_macho_client` & timers.
    let (sys_tx, sys_rx) = std::sync::mpsc::channel();
    let (rt_stop_tx, rt_stop_rx) = std::sync::mpsc::channel::<()>();
    let ctx_actor = Arc::clone(&ctx);
    let actor_thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let h = rt.block_on(async move { ctx_actor.systems.get_or_spawn(sys_id) });
        sys_tx.send(h).unwrap();
        let _ = rt_stop_rx.recv();
    });
    let sys_handle = sys_rx.recv().unwrap();

    let (sel_rsp, _) = call_tcp(
        &mut stream,
        client_id,
        ack.user_id,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    )
    .await;
    assert_eq!(sel_rsp.packet_type, MachoPacketType::CallRsp);

    let (undock_rsp, _) = call_tcp(&mut stream, client_id, ack.user_id, 2, "ship", "Undock", vec![]).await;
    assert_eq!(undock_rsp.packet_type, MachoPacketType::CallRsp);

    let (form_rsp, form_notifs) = call_tcp(
        &mut stream,
        client_id,
        ack.user_id,
        3,
        "beyonce",
        "GetFormations",
        vec![],
    )
    .await;
    assert_eq!(form_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        form_notifs
            .iter()
            .any(|p| packet_has_destiny_action(p, "SetState")),
        "initial GetFormations must deliver SetState"
    );

    // Pause SystemActor and issue an in-space RPC over the live TCP socket.
    let pause_guard = PausedActorGuard::pause(&sys_handle);

    let rpc_start = std::time::Instant::now();
    let (acct_rsp, _) = call_tcp(
        &mut stream,
        client_id,
        ack.user_id,
        4,
        "account",
        "GetCashBalance",
        vec![],
    )
    .await;
    let rpc_elapsed = rpc_start.elapsed();
    assert_eq!(
        acct_rsp.packet_type,
        MachoPacketType::CallRsp,
        "A3: live TCP socket must preserve committed Ok CallRsp on post-RPC ask timeout"
    );
    assert!(
        rpc_elapsed >= POST_RPC_ACTOR_ASK_TIMEOUT.saturating_sub(Duration::from_millis(150))
            && rpc_elapsed < POST_RPC_ACTOR_ASK_TIMEOUT + Duration::from_secs(2),
        "live TCP call must complete right after POST_RPC_ACTOR_ASK_TIMEOUT (got {rpc_elapsed:?})"
    );

    // Resume actor and step ticks without client sending any packet: SetState must arrive on socket
    // within 2 ticks.
    pause_guard.resume();
    let mut saw_set_state_recovery = false;
    'tick_loop: for tick_idx in 0..2 {
        tokio::time::timeout(Duration::from_secs(5), sys_handle.tick())
            .await
            .unwrap_or_else(|_| panic!("sys_handle.tick() timed out on recovery tick {tick_idx}"))
            .expect("step system tick");
        let tick_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while let Ok(Ok(Some(rep))) =
            tokio::time::timeout_at(tick_deadline, read_framed_pyrep(&mut stream)).await
        {
            let pkt = MachoPacket::from_pyrep(&rep).unwrap();
            if packet_has_destiny_action(&pkt, "SetState") {
                saw_set_state_recovery = true;
                break 'tick_loop;
            }
        }
    }
    assert!(
        saw_set_state_recovery,
        "A2/A4: live TCP client must receive SetState within 2 ticks after actor resumes without sending anything"
    );
    drop(stream);
    let _ = rt_stop_tx.send(());
    let _ = actor_thread.join();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn docking_transition_peer_flush_timeout_preserves_pre_dock_destiny_before_session_change() {
    use crate::outbound_sequencer::{sequence, DestinyDrain, RpcCompletion};

    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sid = 91_510_u64;
    let client_id = sid * 10;

    let (ctx, sys_handle, sess) =
        setup_bootstrapped_space_session(sid, sys_id, char_id, ship_id).await;
    let mut sys_rx = sys_handle.subscribe();
    let mut wire_state = SessionWireState::new(sid, client_id);
    let (mut client_rd, mut server_wr) = tokio::io::duplex(64 * 1024);
    let _ep = emit_initial_live_set_state(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        sys_id,
        ship_id,
        false,
    )
    .await;

    // Enqueue a real observer destiny update before docking so `run_pre_dock_observer_ask_bounded`
    // collects a non-empty `pre_dock_destiny` batch while the actor is healthy.
    assert!(
        sys_handle
            .emit_observer_destiny_event(
                ship_id,
                eve_destiny::DestinyEvent::Stop { ball_id: ship_id },
            )
            .await
    );
    let pre_dock_scope =
        crate::outbound_scene_scope::space_scope(Some(sys_id), Some(wire_state.scene_epoch));
    let (pre_dock_destiny, pre_dock_ticks) = run_pre_dock_observer_ask_bounded(
        &ctx,
        &sys_handle,
        &mut wire_state,
        Some(&mut sys_rx),
        sys_id,
        ship_id,
        sid,
        pre_dock_scope,
        Duration::from_millis(200),
    )
    .await
    .expect("healthy pre_dock drain succeeds");
    assert!(
        pre_dock_destiny.is_some(),
        "expected non-empty pre_dock_destiny before Detach"
    );

    // BF-1: pause actor before the Detach peer flush; timeout must NOT mark `AwaitingSetState` or
    // drop already-drained `pre_dock_destiny` before `SessionChangeNotification`.
    let pause_guard = PausedActorGuard::pause(&sys_handle);
    let flush_start = std::time::Instant::now();
    let flush_ok = best_effort_flush_peer_observer_updates_detach(
        sid,
        &sys_handle,
        Some(ship_id),
        Duration::from_millis(80),
        "docking_transition",
    )
    .await;
    assert!(!flush_ok);
    assert!(flush_start.elapsed() < Duration::from_millis(500));
    assert!(
        matches!(wire_state.destiny, DestinyStream::Live { .. }),
        "BF-1: Detach peer flush timeout must keep stream Live until Detach sequencing, got {:?}",
        wire_state.destiny
    );
    assert!(!wire_state.needs_awaiting_set_state_resync());

    let out = sequence(
        &mut wire_state,
        RpcCompletion {
            call_id: 210,
            response_frame: PyRep::Tuple(vec![PyRep::Integer(1)]),
            session_change_frame: Some(PyRep::Tuple(vec![PyRep::String("SessionChange".into())])),
            pending_tick_batches: pre_dock_ticks,
            destiny_drain: DestinyDrain::Detach {
                pre_detach: pre_dock_destiny,
            },
            notifications: Vec::new(),
            item_change_before_session_change: false,
        },
    );
    let destiny_idx = out
        .iter()
        .position(|f| matches!(f, OutFrame::DoDestinyUpdate { .. }))
        .expect("BF-1: pre_dock_destiny must NOT be dropped when Detach peer flush times out");
    let sc_idx = out
        .iter()
        .position(|f| matches!(f, OutFrame::SessionChange { .. }))
        .expect("SessionChange frame present");
    assert!(
        destiny_idx < sc_idx,
        "BF-1: pre_dock_destiny ({destiny_idx}) must precede SessionChange ({sc_idx})"
    );
    assert_eq!(wire_state.destiny, DestinyStream::Detached);
    pause_guard.resume();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upwell_view_outside_undock_and_detached_ship_swap_timeout_recover_on_next_tick() {
    use std::sync::Mutex;

    // 1) Upwell View Outside -> Undock (`BF-2`): `enter_space` clears `last_wire_epoch_and_stamp`,
    //    then `post_rpc` times out while `is_beyonce_ballpark_rpc == false` (`Ok` reply kept).
    //    `awaiting_tick_set_state_resync` must keep `needs_awaiting_set_state_resync() == true`
    //    and deliver `SetState` on the next `Ok(tick_msg)` without any client RPC.
    let ctx = ServerContext::new();
    let sys_id = 30_000_144_u32;
    let char_id = 90_000_001_i64;
    let struct_id = 1_040_001_170_201_i64;
    let sid = 91_511_u64;
    let client_id = sid * 10;
    let user_id = 1_u64;
    let mut ship_id = 0_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).expect("pilot char");
        let corp_id = ch.corporation_id;
        ship_id = ch.active_ship_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = Some(struct_id);
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.location_id = struct_id;
            ship_item.flag = eve_db::InventoryFlag::Hangar;
        }
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                solar_system_id: sys_id,
                owner_corp_id: corp_id,
                name: "GH1170 BF2 Astrahus".into(),
                position: [250_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 2_500_000.0,
                armor_hp: 2_500_000.0,
                hull_hp: 2_500_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), true, ctx.now_unix())
        .expect("acquire online lock");

    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess.structure_id = Some(struct_id);
    sess.live_notify_listener = true;

    let mut sys_rx = sys_handle.subscribe();
    let mut subscribed_sys_id = None;
    let mut wire_state = SessionWireState::new(sid, client_id);
    let mut deferred_ship_id = None;
    let kwargs = HashMap::new();
    let req_source = MachoAddress::Client {
        client_id,
        call_id: 1,
        service: None,
    };
    let beyonce_dest = MachoAddress::Node {
        node_id: eve_machonet::MACHO_PROXY_NODE_ID,
        service: Some("beyonce".to_string()),
        call_id: 0,
    };
    {
        let mut wire_refs = CallReqWireRefs {
            client_id,
            sys_rx: &mut sys_rx,
            subscribed_sys_id: &mut subscribed_sys_id,
            wire_state: &mut wire_state,
            deferred_ship_id: &mut deferred_ship_id,
        };
        let view_out = execute_call_req_wire_with_ask_timeout(
            &ctx,
            &mut sess,
            &mut wire_refs,
            &req_source,
            &beyonce_dest,
            "beyonce",
            None,
            "GetFormations",
            &[],
            &kwargs,
            Duration::from_millis(500),
        )
        .await;
        assert!(view_out.rpc_result.is_ok());
    }
    assert!(
        matches!(wire_state.destiny, DestinyStream::Live { .. }),
        "View Outside must bootstrap Live wire_state, got {:?}",
        wire_state.destiny
    );
    let ep_before_undock = wire_state.current_epoch();

    // Undock from View Outside; pause the actor immediately after `dispatch_rpc` commits Undock.
    let undock_dest = MachoAddress::Node {
        node_id: eve_machonet::MACHO_PROXY_NODE_ID,
        service: Some("stationSvc".to_string()),
        call_id: 0,
    };
    let pause_holder: Arc<Mutex<Option<PausedActorGuard>>> = Arc::new(Mutex::new(None));
    let pause_holder_hook = Arc::clone(&pause_holder);
    let sys_handle_hook = sys_handle.clone();
    set_undock_dispatch_hook_for_test(
        &ctx,
        char_id,
        true,
        Some(Box::new(move || {
            *pause_holder_hook.lock().unwrap() = Some(PausedActorGuard::pause(&sys_handle_hook));
        })),
    );
    let undock_out = {
        let mut wire_refs = CallReqWireRefs {
            client_id,
            sys_rx: &mut sys_rx,
            subscribed_sys_id: &mut subscribed_sys_id,
            wire_state: &mut wire_state,
            deferred_ship_id: &mut deferred_ship_id,
        };
        execute_call_req_wire_with_ask_timeout(
            &ctx,
            &mut sess,
            &mut wire_refs,
            &MachoAddress::Client {
                client_id,
                call_id: 2,
                service: None,
            },
            &undock_dest,
            "stationSvc",
            None,
            "Undock",
            &[],
            &kwargs,
            Duration::from_millis(80),
        )
        .await
    };
    assert!(
        undock_out.rpc_result.is_ok(),
        "Upwell Undock must preserve Ok reply on post-RPC ask timeout: {:?}",
        undock_out.rpc_result
    );
    assert!(
        undock_out
            .out_frames
            .iter()
            .any(|f| matches!(f, OutFrame::SessionChange { .. })),
        "Upwell Undock must still send SessionChangeNotification"
    );
    let ep_after_undock = wire_state.current_epoch();
    assert!(ep_after_undock > ep_before_undock);
    assert!(
        wire_state.needs_awaiting_set_state_resync(),
        "BF-2: Upwell View Outside -> Undock timeout must set awaiting_tick_set_state_resync even after enter_space cleared last_wire_epoch_and_stamp"
    );
    assert_eq!(subscribed_sys_id, Some(sys_id));
    let (mut client_rd, mut server_wr) = tokio::io::duplex(64 * 1024);
    let pause_undock = pause_holder.lock().unwrap().take().unwrap();
    assert_next_tick_recovers_set_state_without_client_rpc(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        ship_id,
        ep_after_undock,
        pause_undock,
    )
    .await;

    // 2) BF-2: `sync_in_space_ship_swap_destiny_bounded` entering from `Detached` and timing out
    //    must also set `needs_awaiting_set_state_resync() == true` and recover on next tick.
    let mut detached_ws = SessionWireState::new(sid + 1, client_id + 1);
    let mut swap_rx = sys_handle.subscribe();
    let pause_swap = PausedActorGuard::pause(&sys_handle);
    let swap_res = sync_in_space_ship_swap_destiny_bounded(
        &ctx,
        &sys_handle,
        &mut swap_rx,
        &mut detached_ws,
        sid + 1,
        sys_id,
        ship_id,
        None,
        false,
        Duration::from_millis(80),
    )
    .await;
    assert!(swap_res.is_none());
    let ep_swap = detached_ws.current_epoch();
    assert!(
        detached_ws.needs_awaiting_set_state_resync(),
        "BF-2: ship swap timeout from Detached must mark stream for next-tick SetState recovery"
    );
    assert_next_tick_recovers_set_state_without_client_rpc(
        &ctx,
        &sys_handle,
        &mut swap_rx,
        &mut detached_ws,
        &mut server_wr,
        &mut client_rd,
        &sess,
        ship_id,
        ep_swap,
        pause_swap,
    )
    .await;

    // 3) Negative control: an unbootstrapped initial `beyonce.GetFormations` timeout (returning
    //    `ErrorResponse`) must leave `needs_awaiting_set_state_resync() == false`.
    let mut unboot_ws = SessionWireState::new(sid + 2, client_id + 2);
    let mut unboot_rx = sys_handle.subscribe();
    let mut unboot_sub_sys = Some(sys_id);
    let mut unboot_def_ship = None;
    let pause_unboot = PausedActorGuard::pause(&sys_handle);
    let unboot_out = {
        let mut wire_refs = CallReqWireRefs {
            client_id: client_id + 2,
            sys_rx: &mut unboot_rx,
            subscribed_sys_id: &mut unboot_sub_sys,
            wire_state: &mut unboot_ws,
            deferred_ship_id: &mut unboot_def_ship,
        };
        execute_call_req_wire_with_ask_timeout(
            &ctx,
            &mut sess,
            &mut wire_refs,
            &req_source,
            &beyonce_dest,
            "beyonce",
            None,
            "GetFormations",
            &[],
            &kwargs,
            Duration::from_millis(80),
        )
        .await
    };
    assert!(unboot_out.rpc_result.is_err());
    assert!(
        !unboot_ws.needs_awaiting_set_state_resync(),
        "BF-2 negative control: initial unbootstrapped beyonce ErrorResponse must NOT emit unsolicited SetState on tick"
    );
    pause_unboot.resume();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepare_dock_warp_settle_bounded_prevents_station_dock_hang_and_recovers() {
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sid = 91_512_u64;
    let client_id = sid * 10;

    let (ctx, sys_handle, sess) =
        setup_bootstrapped_space_session(sid, sys_id, char_id, ship_id).await;
    let mut sys_rx = sys_handle.subscribe();
    let mut wire_state = SessionWireState::new(sid, client_id);
    let (mut client_rd, mut server_wr) = tokio::io::duplex(64 * 1024);
    let ep = emit_initial_live_set_state(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        sys_id,
        ship_id,
        false,
    )
    .await;

    let pause_guard = PausedActorGuard::pause(&sys_handle);
    let started = std::time::Instant::now();
    let res = prepare_dock_warp_settle_bounded(
        &ctx,
        &sess,
        &mut wire_state,
        "stationSvc",
        "Dock",
        &[PyRep::Integer(60_003_760)],
        Duration::from_millis(80),
    )
    .await;
    let err = match res {
        Ok(_) => {
            panic!("BF-3: paused actor must bound prepare_dock_warp_settle instead of hanging")
        }
        Err(err) => err,
    };
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(err.code, "ServiceTemporarilyUnavailable");
    assert!(matches!(
        err.to_client_error("stationSvc", "Dock"),
        eve_proto::ClientError::CustomNotify(ref msg)
            if msg.contains("prepare_dock_warp_settle") && msg.contains("ask timed out")
    ));
    assert_next_tick_recovers_set_state_without_client_rpc(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        &mut wire_state,
        &mut server_wr,
        &mut client_rd,
        &sess,
        ship_id,
        ep,
        pause_guard,
    )
    .await;
}
