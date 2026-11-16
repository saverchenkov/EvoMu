//! Integration & unit tests for GH #1084:
//! - Per-entity materialization gate (`materialized` tracking on `AddBalls2` / `SetState` / `RemoveBalls`,
//!   drop-never-queue semantics for unmaterialized entities, same-dispatch ordering after `AddBalls2`)
//! - Four-invariant Destiny group validator (`single-destiny-stamp-per-notification`,
//!   `group-stamp-matches-update-stamp`, `monotonic-destiny-notification-stamp`,
//!   `remove-after-addballs` with `SetState` reset) in both strict-panic test mode and production emit+count mode
//! - Prevented-jolt critical CRT marker and strict single call-site guarantee (zero `DoDestinyUpdate` emitted)
//! - Observability hooks (`/session ledger <char>` and `--gm` `GET /ops`)
//! - Five `#1077` owner/observer multi-session live journeys through `OwnerObserverFixture`

#![forbid(unsafe_code)]

use crate::common::{
    gm_pilot_in_space, new_wire_harness, owner_observer_fixture, pin_clock, slash_cmd,
    wrap_wire_harness,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, Ballpark, DestinyEvent, MovementMode, SlimItemMeta, TargetLock};
use eve_destiny::wire::{
    encode_add_balls2_action, encode_damage_state_tuple, encode_destiny_event,
    encode_set_state_update, slim_item_dict,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::flight_recorder::{FlightRecorder, OutboundDisposition};
use eve_server::log_ring_buffer::global_log_ring;
use eve_server::outbound_sequencer::{
    sequence_tick_with_events, sequence_with_events, DestinyBatch, DestinyDrain, DestinyStream,
    OutFrame, RpcCompletion, SequencerLedgerEventKind, SessionWireState, TypedNotification,
};
use eve_server::router::{NotificationTarget, ServerContext};
use eve_server::sequencer_invariants::{
    InvariantValidatorMode, INV_GROUP_STAMP_MATCHES_UPDATE_STAMP, INV_MONOTONIC_STAMP,
    INV_REMOVE_AFTER_ADDBALLS, INV_SINGLE_STAMP_PER_NOTIFICATION, REASON_ENTITY_NOT_MATERIALIZED,
};
use eve_server::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};
use eve_server::services::serverops_cluster::{
    clear_manual_system_tidi_override, set_system_tidi_factor,
};
use eve_server::sso::handle_http_request_with_peer;
use eve_server::system_actor::SystemHandle;
use eve_testkit::client_harness::{extract_bound_id_from_wire_rep, ClientWireFrameKind};
use glam::DVec3;
use std::collections::HashMap;
use std::net::IpAddr;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

const JITA_SYS: u32 = 30_000_142;
const MAURASI_SYS: u32 = 30_000_140;
const PERIMETER_SYS: u32 = 30_000_144;
const NULLSEC_SYS: u32 = 30_000_021; // Kuharah (-1.0)

fn make_test_ball(id: i64, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

fn make_slim_rep(ball_id: i64) -> PyRep {
    let ball = make_test_ball(ball_id, DVec3::ZERO);
    let meta = SlimItemMeta {
        group_id: 25,
        category_id: 6,
        owner_id: 90_000_001,
        ..SlimItemMeta::default()
    };
    let mut kv_entries: Vec<(String, PyRep)> = match slim_item_dict(&ball, &meta) {
        PyRep::Dict(entries) => entries
            .into_iter()
            .filter_map(|(k, v)| match k {
                PyRep::String(key) => Some((key, v)),
                _ => None,
            })
            .collect(),
        other => return other,
    };
    for (k, default_val) in [
        ("controllerID", PyRep::None),
        ("incapacitated", PyRep::Bool(false)),
        ("posState", PyRep::Integer(0)),
        ("posTimestamp", PyRep::Integer(0)),
        ("isEmpty", PyRep::Bool(false)),
        ("lootRights", PyRep::None),
        ("hackingSecurityState", PyRep::None),
        ("orbitalHackerID", PyRep::None),
        ("orbitalHackerProgress", PyRep::Real(0.0)),
        ("orbitalState", PyRep::None),
        ("orbitalTimestamp", PyRep::Integer(0)),
    ] {
        if !kv_entries.iter().any(|(existing, _)| existing == k) {
            kv_entries.push((k.to_string(), default_val));
        }
    }
    PyRep::keyval(kv_entries)
}

fn make_update_entry(stamp: u32, action: &str, args: Vec<PyRep>) -> PyRep {
    PyRep::Tuple(vec![
        PyRep::Integer(i64::from(stamp)),
        PyRep::Tuple(vec![PyRep::String(action.into()), PyRep::Tuple(args)]),
    ])
}

fn attach_test_session(
    state: &mut SessionWireState,
    park: &Ballpark,
    observer_ball_id: i64,
    stamp: u32,
) -> u32 {
    let epoch = state.enter_space(Some(JITA_SYS));
    let mut set_state_park = park.clone();
    set_state_park.current_tick = u64::from(stamp);
    let ss_update = encode_set_state_update(&set_state_park, observer_ball_id);
    let outcome = sequence_with_events(
        state,
        RpcCompletion {
            call_id: 1,
            response_frame: PyRep::None,
            session_change_frame: None,
            item_change_before_session_change: false,
            pending_tick_batches: Vec::new(),
            destiny_drain: DestinyDrain::Attach {
                epoch,
                set_state: DestinyBatch::new(epoch, stamp, true, PyRep::List(vec![ss_update])),
            },
            notifications: Vec::new(),
        },
    );
    assert!(
        outcome
            .frames
            .iter()
            .any(|f| matches!(f, OutFrame::DoDestinyUpdate { has_set_state: true, .. })),
        "Attach must emit SetState DoDestinyUpdate: {:?}",
        outcome.frames
    );
    epoch
}

fn fit_module_and_charge(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_item_id: i64,
    module_type_id: u32,
    flag: InventoryFlag,
    charge_opt: Option<(i64, u32, i32)>,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            module_item_id,
            InventoryItemRecord {
                item_id: module_item_id,
                type_id: module_type_id,
                owner_id: char_id,
                location_id: ship_id,
                flag,
                quantity: 1,
                singleton: true,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
        if let Some((charge_id, charge_type_id, qty)) = charge_opt {
            snap.items.insert(
                charge_id,
                InventoryItemRecord {
                    item_id: charge_id,
                    type_id: charge_type_id,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..InventoryItemRecord::default()
                },
            );
        }
    });
}

async fn gm_pilot_docked_in_jita(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    ship_type_id: u32,
) -> (SessionState, i64) {
    let kw = HashMap::new();
    let user = format!("docked{char_id}");
    let pass = format!("{user}-secret");
    ctx.db
        .provision_gm_account(&user, &pass, char_id, &format!("Docked Pilot {char_id}"));
    let mut harness = new_wire_harness(ctx, sess_id);
    harness
        .call_ok(
            "authentication",
            None,
            "Login",
            &[PyRep::String(user), PyRep::String(pass)],
            &kw,
        )
        .await;
    harness
        .call_ok(
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &kw,
        )
        .await;
    let created = harness
        .call_ok(
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(format!("/createitem {ship_type_id} 1"))],
            &kw,
        )
        .await;
    let ship_id = created
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("/createitem itemID");
    let required: Vec<u32> = ctx
        .sde
        .types
        .get(&ship_type_id)
        .map(|t| {
            [182_u16, 183, 184, 1285, 1289, 1290]
                .iter()
                .filter_map(|a| t.default_attributes.get(a))
                .map(|v| v.round() as u32)
                .filter(|&s| s > 0)
                .collect()
        })
        .unwrap_or_default();
    for skill_id in required {
        harness
            .call_ok(
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String(format!("/giveskill {skill_id} 5"))],
                &kw,
            )
            .await;
    }
    ctx.advance_secs(31);
    harness
        .call_ok("ship", None, "Board", &[PyRep::Integer(ship_id)], &kw)
        .await;
    ctx.advance_secs(31);
    let mut sess = harness.into_transport().inner.into_session();
    sess.ship_id = Some(ship_id);
    sess.live_notify_listener = false;
    let _ = sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_id);
    (sess, ship_id)
}

async fn prepare_ship_for_combat(
    sys: &SystemHandle,
    ship_id: i64,
    pos: DVec3,
    hp_override: Option<f64>,
) {
    assert!(
        sys.set_ball_position(ship_id, pos).await,
        "set_ball_position failed for ship {ship_id}"
    );
    let _ = sys
        .modify_combat_state(ship_id, move |cs| {
            cs.undock_invulnerable_remaining_ticks = 0;
            cs.is_cloaked = false;
            if let Some(hp) = hp_override {
                cs.shield_hp = 0.0;
                cs.armor_hp = 0.0;
                cs.structure_hp = hp;
                cs.max_structure_hp = hp.max(100.0);
            }
        })
        .await
        .expect("modify_combat_state");
}

async fn lock_target_sync(sys: &SystemHandle, source_id: i64, target_id: i64) {
    sys.add_target_lock(
        source_id,
        TargetLock {
            target_id,
            lock_time_ms: 500.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: None,
        },
    )
    .await
    .expect("add_target_lock");
}

fn assert_clean_observability(ctx: &ServerContext, context_label: &str) {
    let summary = ctx.flight_recorder.sequencer_observability_summary(20);
    assert_eq!(
        summary.invariant_violations_total, 0,
        "{context_label}: unexpected sequencer invariant violations: {:?}",
        summary.recent_invariant_violations
    );
    assert_eq!(
        summary.prevented_jolts_total, 0,
        "{context_label}: unexpected prevented jolts: {:?}",
        summary.recent_prevented_jolts
    );
    assert_eq!(
        summary.dropped_dependent_notifications_total, 0,
        "{context_label}: unexpected dropped dependent notifications: {:?}",
        summary.dropped_dependent_by_name
    );
}

#[tokio::test]
async fn test_per_entity_materialization_gate_live_and_same_dispatch() {
    // parity: feat:navigation.destiny-updates, destiny:SetState, destiny:AddBalls2, destiny:RemoveBalls, destiny:OnSlimItemChange, destiny:OnSpecialFX, destiny:OnDamageStateChange
    eve_testkit::attest(&[
        "feat:navigation.destiny-updates",
        "destiny:SetState",
        "destiny:AddBalls2",
        "destiny:RemoveBalls",
        "destiny:OnSlimItemChange",
        "destiny:OnSpecialFX",
        "destiny:OnDamageStateChange",
    ]);

    // Part A: deterministic sequencer-level gate, drop-never-queue, same-dispatch ordering, RemoveBalls/SetState reset.
    let fr = Arc::new(FlightRecorder::new());
    let sid = 88_108_401_u64;
    let ship_id = 100_108_401_i64;
    let unmat_ball = 100_108_499_i64;

    let mut park = Ballpark::new(JITA_SYS);
    let ego = make_test_ball(ship_id, DVec3::ZERO);
    park.add_ball(ego);

    let mut wire = SessionWireState::new(sid, 1_000_000_000_000 + sid)
        .with_flight_recorder(Arc::clone(&fr));
    let epoch = attach_test_session(&mut wire, &park, ship_id, 100);
    assert!(wire.materialized.contains(&ship_id));
    assert!(!wire.materialized.contains(&unmat_ball));

    // 1. Negative: entity-dependent notifications targeting non-materialized ball are dropped (never queued).
    let drop_outcome = sequence_with_events(
        &mut wire,
        RpcCompletion {
            call_id: 2,
            response_frame: PyRep::String("ok".into()),
            session_change_frame: None,
            item_change_before_session_change: false,
            pending_tick_batches: Vec::new(),
            destiny_drain: DestinyDrain::None,
            notifications: vec![
                TypedNotification {
                    event: "OnSlimItemChange".into(),
                    args: vec![make_slim_rep(unmat_ball), make_slim_rep(unmat_ball)],
                    service: None,
                },
                TypedNotification {
                    event: "OnSpecialFX".into(),
                    args: vec![
                        PyRep::Integer(unmat_ball),
                        PyRep::None,
                        PyRep::None,
                        PyRep::None,
                        PyRep::None,
                        PyRep::String("effects.Laser".into()),
                    ],
                    service: None,
                },
                TypedNotification {
                    event: "OnDamageStateChange".into(),
                    args: vec![
                        PyRep::Integer(unmat_ball),
                        encode_damage_state_tuple(None, 100),
                    ],
                    service: None,
                },
                TypedNotification {
                    event: "OnDroneStateChange".into(),
                    args: vec![PyRep::Integer(unmat_ball)],
                    service: None,
                },
                TypedNotification {
                    event: "OnTarget".into(),
                    args: vec![
                        PyRep::String("add".into()),
                        PyRep::Integer(unmat_ball),
                        PyRep::None,
                    ],
                    service: None,
                },
                // Non-adding target clear ("lost") is intentionally NOT blocked even if entity left.
                TypedNotification {
                    event: "OnTarget".into(),
                    args: vec![
                        PyRep::String("lost".into()),
                        PyRep::Integer(unmat_ball),
                        PyRep::None,
                    ],
                    service: None,
                },
            ],
        },
    );

    let notif_events: Vec<&str> = drop_outcome
        .frames
        .iter()
        .filter_map(|f| match f {
            OutFrame::Notification { event, .. } => Some(event.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        notif_events,
        vec!["OnTarget"],
        "only non-adding OnTarget('lost') should pass when {unmat_ball} is unmaterialized; got {notif_events:?}"
    );
    let dropped_reasons: Vec<(&str, Option<&str>)> = drop_outcome
        .events
        .iter()
        .filter(|e| e.kind == SequencerLedgerEventKind::Dropped)
        .map(|e| (e.reason.as_str(), e.packet_kind.as_deref()))
        .collect();
    assert_eq!(dropped_reasons.len(), 5);
    for (reason, pkt_kind) in &dropped_reasons {
        assert!(
            reason.starts_with(REASON_ENTITY_NOT_MATERIALIZED),
            "unexpected drop reason: {reason}"
        );
        assert!(pkt_kind.is_some());
    }
    let counters = fr.session_sequencer_counters(sid);
    assert_eq!(counters.dropped_dependent_notifications, 5);
    assert_eq!(counters.dropped_dependent_by_name.get("OnSlimItemChange"), Some(&1));
    assert_eq!(counters.dropped_dependent_by_name.get("OnSpecialFX"), Some(&1));
    assert_eq!(counters.dropped_dependent_by_name.get("OnDamageStateChange"), Some(&1));
    assert_eq!(counters.dropped_dependent_by_name.get("OnDroneStateChange"), Some(&1));
    assert_eq!(counters.dropped_dependent_by_name.get("OnTarget"), Some(&1));

    // 2. Same-dispatch materialization: AddBalls2 + OnSlimItemChange in the SAME RpcCompletion
    //    must emit DoDestinyUpdate(AddBalls2) BEFORE OnSlimItemChange, and never resurrect earlier drops.
    let target_ball = make_test_ball(unmat_ball, DVec3::new(2_000.0, 0.0, 0.0));
    park.add_ball(target_ball.clone());
    let add2 = encode_add_balls2_action(101, &[&target_ball], Some(&park));
    let same_dispatch = sequence_with_events(
        &mut wire,
        RpcCompletion {
            call_id: 3,
            response_frame: PyRep::None,
            session_change_frame: None,
            item_change_before_session_change: false,
            pending_tick_batches: Vec::new(),
            destiny_drain: DestinyDrain::Updates(DestinyBatch::new(
                epoch,
                101,
                false,
                PyRep::List(vec![add2]),
            )),
            notifications: vec![TypedNotification {
                event: "OnSlimItemChange".into(),
                args: vec![make_slim_rep(unmat_ball), make_slim_rep(unmat_ball)],
                service: None,
            }],
        },
    );
    assert!(wire.materialized.contains(&unmat_ball));
    let destiny_idx = same_dispatch
        .frames
        .iter()
        .position(|f| matches!(f, OutFrame::DoDestinyUpdate { .. }))
        .expect("same-dispatch must emit DoDestinyUpdate(AddBalls2)");
    let slim_positions: Vec<usize> = same_dispatch
        .frames
        .iter()
        .enumerate()
        .filter_map(|(i, f)| match f {
            OutFrame::Notification { event, .. } if event == "OnSlimItemChange" => Some(i),
            _ => None,
        })
        .collect();
    assert_eq!(
        slim_positions.len(),
        1,
        "previously dropped OnSlimItemChange must NOT be queued/replayed upon materialization"
    );
    assert!(
        destiny_idx < slim_positions[0],
        "AddBalls2 DoDestinyUpdate (idx={destiny_idx}) must precede OnSlimItemChange (idx={})",
        slim_positions[0]
    );

    // 3. RemoveBalls evicts entity from `materialized` and causes subsequent updates to drop again.
    let rm_action = encode_destiny_event(
        102,
        &DestinyEvent::RemoveBalls {
            ball_ids: vec![unmat_ball],
        },
        Some(&park),
    );
    let after_rm = sequence_with_events(
        &mut wire,
        RpcCompletion {
            call_id: 4,
            response_frame: PyRep::None,
            session_change_frame: None,
            item_change_before_session_change: false,
            pending_tick_batches: Vec::new(),
            destiny_drain: DestinyDrain::Updates(DestinyBatch::new(
                epoch,
                102,
                false,
                PyRep::List(vec![rm_action]),
            )),
            notifications: vec![TypedNotification {
                event: "OnDamageStateChange".into(),
                args: vec![
                    PyRep::Integer(unmat_ball),
                    encode_damage_state_tuple(None, 102),
                ],
                service: None,
            }],
        },
    );
    assert!(!wire.materialized.contains(&unmat_ball));
    assert!(
        !after_rm
            .frames
            .iter()
            .any(|f| matches!(f, OutFrame::Notification { .. })),
        "post-RemoveBalls notification must be dropped"
    );
    assert_eq!(fr.session_sequencer_counters(sid).dropped_dependent_notifications, 6);

    // 4. SetState resets `materialized` strictly to the entities present in that SetState.
    let extra_ball_id = 100_108_477_i64;
    let extra_ball = make_test_ball(extra_ball_id, DVec3::new(3_000.0, 0.0, 0.0));
    park.add_ball(extra_ball.clone());
    let add_extra = encode_add_balls2_action(103, &[&extra_ball], Some(&park));
    let (extra_frame, _) = sequence_tick_with_events(
        &mut wire,
        DestinyBatch::new(epoch, 103, false, PyRep::List(vec![add_extra])),
    );
    assert!(extra_frame.is_some());
    assert!(wire.materialized.contains(&extra_ball_id));

    let mut reset_park = Ballpark::new(JITA_SYS);
    reset_park.current_tick = 104;
    reset_park.add_ball(make_test_ball(ship_id, DVec3::ZERO));
    let reset_ss = encode_set_state_update(&reset_park, ship_id);
    let _ = sequence_with_events(
        &mut wire,
        RpcCompletion {
            call_id: 5,
            response_frame: PyRep::None,
            session_change_frame: None,
            item_change_before_session_change: false,
            pending_tick_batches: Vec::new(),
            destiny_drain: DestinyDrain::Updates(DestinyBatch::new(
                epoch,
                104,
                true,
                PyRep::List(vec![reset_ss]),
            )),
            notifications: Vec::new(),
        },
    );
    assert!(wire.materialized.contains(&ship_id));
    assert!(
        !wire.materialized.contains(&extra_ball_id),
        "SetState must reset materialized to only SetState entities"
    );
    wire.leave_space();
    assert!(wire.materialized.is_empty());

    // Part B: live wire path (`ServerContext` + `wrap_wire_harness` + `SystemHandle`).
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let pilot = gm_pilot_in_space(&ctx, 88_108_402, 88_108_402, 587, JITA_SYS).await;
    let live_sid = pilot.sess.session_id;
    let live_cid = pilot.char_id;
    let sys_jita = ctx.systems.get_or_spawn(JITA_SYS);
    let (_, pilot_ball) = sys_jita
        .get_ball_snapshot(pilot.ship_id)
        .await
        .unwrap()
        .expect("pilot ship ball");
    let mut harness = wrap_wire_harness(&ctx, pilot.sess);

    let ghost_ball = 88_999_001_i64;
    assert!(!harness.transport().inner.wire_state.materialized.contains(&ghost_ball));

    ctx.notify(
        NotificationTarget::Character(live_cid),
        "OnSlimItemChange",
        "charid",
        vec![make_slim_rep(ghost_ball), make_slim_rep(ghost_ball)],
    );
    let pre_frames = harness.drain_pending_frames();
    assert!(
        !pre_frames.iter().any(|f| matches!(
            &f.kind,
            ClientWireFrameKind::Notification { event, .. } if event == "OnSlimItemChange"
        )),
        "live wire must drop OnSlimItemChange for unmaterialized ball {ghost_ball}"
    );
    let live_counters = ctx.flight_recorder.session_sequencer_counters(live_sid);
    assert_eq!(live_counters.dropped_dependent_notifications, 1);
    assert_eq!(
        live_counters.dropped_dependent_by_name.get("OnSlimItemChange"),
        Some(&1)
    );
    let ledger = ctx.flight_recorder.get_outbound_ledger(live_sid, 20);
    assert!(
        ledger.iter().any(|e| {
            e.disposition == OutboundDisposition::Dropped
                && e.reason.starts_with(REASON_ENTITY_NOT_MATERIALIZED)
                && e.packet_kind == "OnSlimItemChange"
        }),
        "outbound ledger missing materialization drop: {ledger:?}"
    );

    // Spawn `ghost_ball` near pilot, tick once, verify AddBalls2 arrives WITHOUT replaying earlier drop.
    sys_jita
        .spawn_ball(make_test_ball(
            ghost_ball,
            pilot_ball.position + DVec3::new(1_000.0, 0.0, 0.0),
        ))
        .await
        .expect("spawn ghost_ball");
    sys_jita.step_manual(1.0).await.unwrap();
    let add_frames = harness.drain_pending_frames();
    assert!(
        add_frames
            .iter()
            .any(|f| matches!(&f.kind, ClientWireFrameKind::DoDestinyUpdate { .. })),
        "expected DoDestinyUpdate with AddBalls2 after spawning ghost_ball"
    );
    assert!(
        !add_frames.iter().any(|f| matches!(
            &f.kind,
            ClientWireFrameKind::Notification { event, .. } if event == "OnSlimItemChange"
        )),
        "dropped notification must never replay once entity materializes"
    );
    assert!(harness.transport().inner.wire_state.materialized.contains(&ghost_ball));

    // Fresh OnSlimItemChange now passes because `ghost_ball` is materialized.
    ctx.notify(
        NotificationTarget::Character(live_cid),
        "OnSlimItemChange",
        "charid",
        vec![make_slim_rep(ghost_ball), make_slim_rep(ghost_ball)],
    );
    let post_add_frames = harness.drain_pending_frames();
    assert!(
        post_add_frames.iter().any(|f| matches!(
            &f.kind,
            ClientWireFrameKind::Notification { event, .. } if event == "OnSlimItemChange"
        )),
        "fresh OnSlimItemChange after AddBalls2 must be delivered"
    );

    // Remove ghost_ball -> tick -> RemoveBalls evicts it -> next OnSpecialFX drops.
    let _ = sys_jita.remove_ball(ghost_ball).await;
    sys_jita.step_manual(1.0).await.unwrap();
    let _ = harness.drain_pending_frames();
    assert!(!harness.transport().inner.wire_state.materialized.contains(&ghost_ball));

    ctx.notify(
        NotificationTarget::Character(live_cid),
        "OnSpecialFX",
        "charid",
        vec![
            PyRep::Integer(ghost_ball),
            PyRep::None,
            PyRep::None,
            PyRep::None,
            PyRep::None,
            PyRep::String("effects.Laser".into()),
        ],
    );
    let post_rm_frames = harness.drain_pending_frames();
    assert!(
        !post_rm_frames.iter().any(|f| matches!(
            &f.kind,
            ClientWireFrameKind::Notification { event, .. } if event == "OnSpecialFX"
        )),
        "post-RemoveBalls OnSpecialFX must be dropped"
    );
    assert_eq!(
        ctx.flight_recorder
            .session_sequencer_counters(live_sid)
            .dropped_dependent_notifications,
        2
    );
}

#[test]
fn test_destiny_group_invariant_validator_all_four_invariants_and_modes() {
    // parity: feat:navigation.destiny-updates, destiny:AddBalls2, destiny:RemoveBalls, destiny:SetState
    eve_testkit::attest(&[
        "feat:navigation.destiny-updates",
        "destiny:AddBalls2",
        "destiny:RemoveBalls",
        "destiny:SetState",
    ]);

    let ego_id = 100_208_401_i64;
    let victim_id = 100_208_402_i64;
    let mut park = Ballpark::new(JITA_SYS);
    let ego = make_test_ball(ego_id, DVec3::ZERO);
    let victim = make_test_ball(victim_id, DVec3::new(1_500.0, 0.0, 0.0));
    park.add_ball(ego);
    park.add_ball(victim.clone());

    // 1. Positive suite under default StrictPanic mode (`cfg!(any(test, feature = "testkit"))`).
    let fr_pos = Arc::new(FlightRecorder::new());
    let mut pos_wire = SessionWireState::new(88_208_400, 1_000_000_000_001)
        .with_flight_recorder(Arc::clone(&fr_pos));
    assert_eq!(pos_wire.invariant_validator_mode(), InvariantValidatorMode::StrictPanic);
    let ep_pos = attach_test_session(&mut pos_wire, &park, ego_id, 200);

    // Same-stamp multi-update group + matching stamp + monotonic + RemoveBalls > AddBalls2 stamp.
    let add2_200 = encode_add_balls2_action(200, &[&victim], Some(&park));
    let stop_200 = make_update_entry(200, "Stop", vec![PyRep::Integer(ego_id)]);
    let (f_add, _) = sequence_tick_with_events(
        &mut pos_wire,
        DestinyBatch::new(ep_pos, 200, false, PyRep::List(vec![add2_200, stop_200])),
    );
    assert!(f_add.is_some());

    let rm_201 = encode_destiny_event(
        201,
        &DestinyEvent::RemoveBalls {
            ball_ids: vec![victim_id],
        },
        Some(&park),
    );
    let (f_rm, _) = sequence_tick_with_events(
        &mut pos_wire,
        DestinyBatch::new(ep_pos, 201, false, PyRep::List(vec![rm_201])),
    );
    assert!(f_rm.is_some());

    // SetState reset check: AddBalls2 at stamp 205 -> SetState at 206 resets last_addballs_stamp -> RemoveBalls at 206 passes.
    let add2_205 = encode_add_balls2_action(205, &[&victim], Some(&park));
    let _ = sequence_tick_with_events(
        &mut pos_wire,
        DestinyBatch::new(ep_pos, 205, false, PyRep::List(vec![add2_205])),
    );
    let mut ss_park = park.clone();
    ss_park.current_tick = 206;
    let ss_206 = encode_set_state_update(&ss_park, ego_id);
    let rm_206 = encode_destiny_event(
        206,
        &DestinyEvent::RemoveBalls {
            ball_ids: vec![victim_id],
        },
        Some(&park),
    );
    let (f_reset, _) = sequence_tick_with_events(
        &mut pos_wire,
        DestinyBatch::new(ep_pos, 206, true, PyRep::List(vec![ss_206, rm_206])),
    );
    assert!(f_reset.is_some());
    assert_eq!(fr_pos.session_sequencer_counters(88_208_400).invariant_violations, 0);

    // 2. Negative tests for ALL 4 invariants in BOTH StrictPanic (test default -> panic + record)
    //    AND RecordAndEmit (production -> emit frame + increment counter + record in FlightRecorder).
    for mode in [
        InvariantValidatorMode::StrictPanic,
        InvariantValidatorMode::RecordAndEmit,
    ] {
        // (a) `single-destiny-stamp-per-notification`
        {
            let sid = match mode {
                InvariantValidatorMode::StrictPanic => 88_208_411,
                InvariantValidatorMode::RecordAndEmit => 88_208_421,
            };
            let fr = Arc::new(FlightRecorder::new());
            let mut wire = SessionWireState::new(sid, 1).with_flight_recorder(Arc::clone(&fr));
            let ep = attach_test_session(&mut wire, &park, ego_id, 100);
            wire.set_invariant_validator_mode(mode);
            let mixed = PyRep::List(vec![
                make_update_entry(101, "Stop", vec![PyRep::Integer(ego_id)]),
                make_update_entry(102, "Stop", vec![PyRep::Integer(ego_id)]),
            ]);
            let batch = DestinyBatch::new(ep, 101, false, mixed);
            match mode {
                InvariantValidatorMode::StrictPanic => {
                    let res = catch_unwind(AssertUnwindSafe(|| {
                        sequence_tick_with_events(&mut wire, batch)
                    }));
                    assert!(res.is_err(), "StrictPanic must panic on {INV_SINGLE_STAMP_PER_NOTIFICATION}");
                }
                InvariantValidatorMode::RecordAndEmit => {
                    let (frame_opt, _) = sequence_tick_with_events(&mut wire, batch);
                    assert!(frame_opt.is_some(), "RecordAndEmit must still emit frame");
                }
            }
            let c = fr.session_sequencer_counters(sid);
            assert!(
                c.invariant_violations_by_name
                    .get(INV_SINGLE_STAMP_PER_NOTIFICATION)
                    .copied()
                    .unwrap_or(0)
                    >= 1
            );
            assert!(
                fr.get_session_invariant_violations(sid, 10)
                    .iter()
                    .any(|v| v.invariant == INV_SINGLE_STAMP_PER_NOTIFICATION)
            );
        }

        // (b) `group-stamp-matches-update-stamp`
        {
            let sid = match mode {
                InvariantValidatorMode::StrictPanic => 88_208_412,
                InvariantValidatorMode::RecordAndEmit => 88_208_422,
            };
            let fr = Arc::new(FlightRecorder::new());
            let mut wire = SessionWireState::new(sid, 2).with_flight_recorder(Arc::clone(&fr));
            let ep = attach_test_session(&mut wire, &park, ego_id, 100);
            wire.set_invariant_validator_mode(mode);
            let mismatch = PyRep::List(vec![make_update_entry(
                99,
                "Stop",
                vec![PyRep::Integer(ego_id)],
            )]);
            let batch = DestinyBatch::new(ep, 101, false, mismatch);
            match mode {
                InvariantValidatorMode::StrictPanic => {
                    let res = catch_unwind(AssertUnwindSafe(|| {
                        sequence_tick_with_events(&mut wire, batch)
                    }));
                    assert!(res.is_err(), "StrictPanic must panic on {INV_GROUP_STAMP_MATCHES_UPDATE_STAMP}");
                }
                InvariantValidatorMode::RecordAndEmit => {
                    let (frame_opt, _) = sequence_tick_with_events(&mut wire, batch);
                    assert!(frame_opt.is_some(), "RecordAndEmit must still emit frame");
                }
            }
            let c = fr.session_sequencer_counters(sid);
            assert_eq!(
                c.invariant_violations_by_name
                    .get(INV_GROUP_STAMP_MATCHES_UPDATE_STAMP),
                Some(&1)
            );
            assert_eq!(
                fr.get_session_invariant_violations(sid, 10)
                    .into_iter()
                    .filter(|v| v.invariant == INV_GROUP_STAMP_MATCHES_UPDATE_STAMP)
                    .count(),
                1
            );
        }

        // (c) `monotonic-destiny-notification-stamp`
        {
            let sid = match mode {
                InvariantValidatorMode::StrictPanic => 88_208_413,
                InvariantValidatorMode::RecordAndEmit => 88_208_423,
            };
            let fr = Arc::new(FlightRecorder::new());
            let mut wire = SessionWireState::new(sid, 3).with_flight_recorder(Arc::clone(&fr));
            let ep = attach_test_session(&mut wire, &park, ego_id, 150);
            wire.set_invariant_validator_mode(mode);
            let regressed = PyRep::List(vec![make_update_entry(
                149,
                "Stop",
                vec![PyRep::Integer(ego_id)],
            )]);
            match mode {
                InvariantValidatorMode::StrictPanic => {
                    let res = catch_unwind(AssertUnwindSafe(|| {
                        wire.validate_outgoing_destiny_frame(ep, 149, &regressed)
                    }));
                    assert!(res.is_err(), "StrictPanic must panic on {INV_MONOTONIC_STAMP}");
                }
                InvariantValidatorMode::RecordAndEmit => {
                    let violated = wire.validate_outgoing_destiny_frame(ep, 149, &regressed);
                    assert_eq!(violated, vec![INV_MONOTONIC_STAMP.to_string()]);
                }
            }
            let c = fr.session_sequencer_counters(sid);
            assert_eq!(c.invariant_violations_by_name.get(INV_MONOTONIC_STAMP), Some(&1));
            assert_eq!(fr.get_session_invariant_violations(sid, 10)[0].stamp, 149);
        }

        // (d) `remove-after-addballs` (`RemoveBalls` at stamp <= last `AddBalls2` stamp)
        {
            let sid = match mode {
                InvariantValidatorMode::StrictPanic => 88_208_414,
                InvariantValidatorMode::RecordAndEmit => 88_208_424,
            };
            let fr = Arc::new(FlightRecorder::new());
            let mut wire = SessionWireState::new(sid, 4).with_flight_recorder(Arc::clone(&fr));
            let ep = attach_test_session(&mut wire, &park, ego_id, 159);
            let add2_160 = encode_add_balls2_action(160, &[&victim], Some(&park));
            let _ = sequence_tick_with_events(
                &mut wire,
                DestinyBatch::new(ep, 160, false, PyRep::List(vec![add2_160])),
            );
            wire.set_invariant_validator_mode(mode);

            let rm_same_stamp = encode_destiny_event(
                160,
                &DestinyEvent::RemoveBalls {
                    ball_ids: vec![victim_id],
                },
                Some(&park),
            );
            let batch = DestinyBatch::new(ep, 160, false, PyRep::List(vec![rm_same_stamp]));
            match mode {
                InvariantValidatorMode::StrictPanic => {
                    let res = catch_unwind(AssertUnwindSafe(|| {
                        sequence_tick_with_events(&mut wire, batch)
                    }));
                    assert!(res.is_err(), "StrictPanic must panic on {INV_REMOVE_AFTER_ADDBALLS}");
                }
                InvariantValidatorMode::RecordAndEmit => {
                    let (frame_opt, _) = sequence_tick_with_events(&mut wire, batch);
                    assert!(
                        frame_opt.is_some(),
                        "RecordAndEmit must count+record and still emit frame"
                    );
                }
            }
            let c = fr.session_sequencer_counters(sid);
            assert_eq!(c.invariant_violations_by_name.get(INV_REMOVE_AFTER_ADDBALLS), Some(&1));
            assert!(
                fr.get_session_invariant_violations(sid, 10)
                    .iter()
                    .any(|v| v.invariant == INV_REMOVE_AFTER_ADDBALLS)
            );
        }
    }
}

#[test]
fn test_prevented_jolt_emits_crt_marker_and_drops_destiny_frame() {
    // parity: feat:navigation.destiny-updates, feat:serverops.server-tick
    eve_testkit::attest(&["feat:navigation.destiny-updates", "feat:serverops.server-tick"]);

    let sid = 98_108_477_u64;
    let ego_id = 100_308_401_i64;
    let mut park = Ballpark::new(JITA_SYS);
    park.add_ball(make_test_ball(ego_id, DVec3::ZERO));

    let fr = Arc::new(FlightRecorder::new());
    let mut wire = SessionWireState::new(sid, 1_000_000_000_777)
        .with_flight_recorder(Arc::clone(&fr));
    let ep = attach_test_session(&mut wire, &park, ego_id, 300);

    let (ok_frame, _) = sequence_tick_with_events(
        &mut wire,
        DestinyBatch::new(
            ep,
            305,
            false,
            PyRep::List(vec![make_update_entry(
                305,
                "Stop",
                vec![PyRep::Integer(ego_id)],
            )]),
        ),
    );
    assert!(ok_frame.is_some());

    // Path A: RPC completion carrying a backstepping batch (304 < 305) drops ONLY DoDestinyUpdate,
    // preserves CallRsp, records prevented jolt, and appends a CRT entry to global_log_ring.
    let rpc_out = sequence_with_events(
        &mut wire,
        RpcCompletion {
            call_id: 77,
            response_frame: PyRep::String("call_rsp_ok".into()),
            session_change_frame: None,
            item_change_before_session_change: false,
            pending_tick_batches: Vec::new(),
            destiny_drain: DestinyDrain::Updates(DestinyBatch::new(
                ep,
                304,
                false,
                PyRep::List(vec![make_update_entry(
                    304,
                    "Stop",
                    vec![PyRep::Integer(ego_id)],
                )]),
            )),
            notifications: Vec::new(),
        },
    );
    assert!(
        !rpc_out
            .frames
            .iter()
            .any(|f| matches!(f, OutFrame::DoDestinyUpdate { .. })),
        "backstepping batch must emit ZERO DoDestinyUpdate frames"
    );
    assert_eq!(
        rpc_out.frames,
        vec![OutFrame::CallRsp {
            call_id: 77,
            frame: PyRep::String("call_rsp_ok".into()),
        }]
    );
    assert_eq!(rpc_out.events.len(), 1);
    assert_eq!(rpc_out.events[0].kind, SequencerLedgerEventKind::Dropped);
    assert!(rpc_out.events[0].reason.starts_with("stamp_regression:304<305"));

    // Path B: tick backstep below SetState/last trusted stamp (303 < 305) also drops + logs CRT.
    let (tick_opt, tick_events) = sequence_tick_with_events(
        &mut wire,
        DestinyBatch::new(
            ep,
            303,
            false,
            PyRep::List(vec![make_update_entry(
                303,
                "Stop",
                vec![PyRep::Integer(ego_id)],
            )]),
        ),
    );
    assert!(tick_opt.is_none());
    assert_eq!(tick_events.len(), 1);
    assert_eq!(tick_events[0].kind, SequencerLedgerEventKind::Dropped);

    assert_eq!(wire.sequencer_counters().prevented_jolts, 2);
    let jolts = fr.get_session_prevented_jolts(sid, 10);
    assert_eq!(jolts.len(), 2);
    assert_eq!(jolts[0].batch_stamp, 304);
    assert_eq!(jolts[0].trusted_stamp, 305);
    assert_eq!(jolts[1].batch_stamp, 303);
    assert_eq!(jolts[1].trusted_stamp, 305);

    let session_tag = format!("session={sid} ");
    let crt_lines: Vec<String> = global_log_ring()
        .snapshot_recent(500)
        .into_iter()
        .filter(|line| {
            line.contains("Z CRT   [outbound_sequencer::prevented_jolt]")
                && line.contains(&session_tag)
        })
        .collect();
    assert_eq!(
        crt_lines.len(),
        2,
        "expected exactly 2 CRT log ring lines for session {sid}, got {crt_lines:?}"
    );
}

#[tokio::test]
async fn test_observability_session_ledger_and_ops_report_sequencer_counters() {
    // parity: feat:navigation.destiny-updates, feat:serverops.server-tick
    eve_testkit::attest(&["feat:navigation.destiny-updates", "feat:serverops.server-tick"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);

    let pilot = gm_pilot_in_space(&ctx, 88_408_401, 88_408_401, 587, JITA_SYS).await;
    let mut admin = gm_pilot_in_space(&ctx, 88_408_402, 88_408_402, 587, JITA_SYS).await;
    let sys_jita = ctx.systems.get_or_spawn(JITA_SYS);
    for _ in 0..3 {
        sys_jita.step_manual(1.0).await.unwrap();
    }

    let mut harness = wrap_wire_harness(&ctx, pilot.sess);
    let bind_rep = harness
        .call_ok(
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(i64::from(JITA_SYS)), PyRep::None],
            &HashMap::new(),
        )
        .await;
    assert!(extract_bound_id_from_wire_rep(&bind_rep).is_some());

    let (epoch, trusted_stamp) = match harness.transport().inner.wire_state.destiny {
        DestinyStream::Live {
            epoch,
            set_state_stamp,
            last_stamp,
        } => (epoch, set_state_stamp.max(last_stamp)),
        other => panic!("expected Live destiny stream after MachoBindObject, got {other:?}"),
    };
    assert!(trusted_stamp >= 3, "expected positive trusted_stamp >= 3, got {trusted_stamp}");

    // 1. Trigger 2 dropped dependent notifications on pilot's wire session.
    let unmat_id = 88_777_404_i64;
    ctx.notify(
        NotificationTarget::Character(pilot.char_id),
        "OnSlimItemChange",
        "charid",
        vec![make_slim_rep(unmat_id), make_slim_rep(unmat_id)],
    );
    ctx.notify(
        NotificationTarget::Character(pilot.char_id),
        "OnSpecialFX",
        "charid",
        vec![
            PyRep::Integer(unmat_id),
            PyRep::None,
            PyRep::None,
            PyRep::None,
            PyRep::None,
            PyRep::String("effects.Laser".into()),
        ],
    );
    let _ = harness.drain_pending_frames();

    // 2. Switch session guard to RecordAndEmit and emit 1 validator mismatch (non-backstepping stamp).
    {
        let wire = &mut harness.transport_mut().inner.wire_state;
        wire.set_invariant_validator_mode(InvariantValidatorMode::RecordAndEmit);
        let mismatch = PyRep::List(vec![make_update_entry(
            trusted_stamp.saturating_sub(1),
            "Stop",
            vec![PyRep::Integer(pilot.ship_id)],
        )]);
        let (emitted, _) = sequence_tick_with_events(
            wire,
            DestinyBatch::new(epoch, trusted_stamp, false, mismatch),
        );
        assert!(emitted.is_some());

        // 3. Force 1 prevented jolt at stamp `trusted_stamp - 1`.
        let backstep = PyRep::List(vec![make_update_entry(
            trusted_stamp - 1,
            "Stop",
            vec![PyRep::Integer(pilot.ship_id)],
        )]);
        let (dropped_jolt, _) = sequence_tick_with_events(
            wire,
            DestinyBatch::new(epoch, trusted_stamp - 1, false, backstep),
        );
        assert!(dropped_jolt.is_none());
    }

    // 4. Query `/session ledger <pilot.char_id> 20` from separate GM session AND verify all fields.
    let ledger_rep = slash_cmd(
        &ctx,
        &mut admin.sess,
        &format!("/session ledger {} 20", pilot.char_id),
    )
    .await;
    assert_eq!(
        ledger_rep
            .dict_get("droppedDependentNotifications")
            .and_then(|v| v.as_i64()),
        Some(2)
    );
    let dropped_by_name = ledger_rep
        .dict_get("droppedDependentByName")
        .expect("droppedDependentByName");
    assert_eq!(
        dropped_by_name
            .dict_get("OnSlimItemChange")
            .and_then(|v| v.as_i64()),
        Some(1)
    );
    assert_eq!(
        dropped_by_name
            .dict_get("OnSpecialFX")
            .and_then(|v| v.as_i64()),
        Some(1)
    );
    assert_eq!(
        ledger_rep
            .dict_get("invariantViolations")
            .and_then(|v| v.as_i64()),
        Some(1)
    );
    assert_eq!(
        ledger_rep
            .dict_get("invariantViolationsByName")
            .and_then(|v| v.dict_get(INV_GROUP_STAMP_MATCHES_UPDATE_STAMP))
            .and_then(|v| v.as_i64()),
        Some(1)
    );
    assert_eq!(
        ledger_rep
            .dict_get("preventedJolts")
            .and_then(|v| v.as_i64()),
        Some(1)
    );
    match ledger_rep.dict_get("invariantViolationEntries") {
        Some(PyRep::List(entries)) => {
            assert_eq!(entries.len(), 1);
            assert_eq!(
                entries[0].dict_get("invariant").and_then(|v| v.as_str()),
                Some(INV_GROUP_STAMP_MATCHES_UPDATE_STAMP)
            );
        }
        other => panic!("expected invariantViolationEntries list, got {other:?}"),
    }
    match ledger_rep.dict_get("preventedJoltEntries") {
        Some(PyRep::List(entries)) => {
            assert_eq!(entries.len(), 1);
            assert_eq!(
                entries[0].dict_get("trustedStamp").and_then(|v| v.as_i64()),
                Some(i64::from(trusted_stamp))
            );
        }
        other => panic!("expected preventedJoltEntries list, got {other:?}"),
    }

    // 5. Query loopback `--gm` `GET /ops` and verify cluster `sequencer` summary.
    ctx.set_gm_ops_enabled(true);
    let loopback: IpAddr = "127.0.0.1".parse().unwrap();
    let resp = handle_http_request_with_peer(&ctx, "GET", "/ops", &HashMap::new(), b"", Some(loopback));
    assert_eq!(resp.status_code, 200);
    let body: serde_json::Value =
        serde_json::from_slice(&resp.body).expect("/ops valid JSON body");
    assert_eq!(body["sequencer"]["dropped_dependent_notifications_total"], 2);
    assert_eq!(body["sequencer"]["dropped_dependent_by_name"]["OnSlimItemChange"], 1);
    assert_eq!(body["sequencer"]["dropped_dependent_by_name"]["OnSpecialFX"], 1);
    assert_eq!(body["sequencer"]["invariant_violations_total"], 1);
    assert_eq!(
        body["sequencer"]["invariant_violations_by_name"][INV_GROUP_STAMP_MATCHES_UPDATE_STAMP],
        1
    );
    assert_eq!(body["sequencer"]["prevented_jolts_total"], 1);
    assert_eq!(
        body["sequencer"]["recent_invariant_violations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        body["sequencer"]["recent_prevented_jolts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn test_five_owner_observer_live_journeys_monotonic_and_clean() {
    // parity: feat:navigation.destiny-updates, feat:serverops.time-dilation, destiny:SetState, destiny:AddBalls2, destiny:RemoveBalls, rpc:beyonce.CmdFollowBall, rpc:beyonce.CmdOrbit, rpc:beyonce.CmdStop, rpc:beyonce.CmdWarpToStuff, rpc:beyonce.CmdStargateJump
    eve_testkit::attest(&[
        "feat:navigation.destiny-updates",
        "feat:serverops.time-dilation",
        "destiny:SetState",
        "destiny:AddBalls2",
        "destiny:RemoveBalls",
        "rpc:beyonce.CmdFollowBall",
        "rpc:beyonce.CmdOrbit",
        "rpc:beyonce.CmdStop",
        "rpc:beyonce.CmdWarpToStuff",
        "rpc:beyonce.CmdStargateJump",
    ]);

    let kw = HashMap::new();

    // Journey 1: Jita dock -> stationSvc.Undock -> CmdFollowBall -> CmdOrbit -> CmdStop
    {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let (owner_sess, owner_ship) = gm_pilot_docked_in_jita(&ctx, 89_001_001, 89_001_001, 587).await;
        let (obs_sess, obs_ship) = gm_pilot_docked_in_jita(&ctx, 89_001_002, 89_001_002, 587).await;
        let mut fixture = owner_observer_fixture(&ctx, owner_sess, obs_sess);
        let sys_jita = ctx.systems.get_or_spawn(JITA_SYS);
        sys_jita.step_manual(1.0).await.unwrap();

        ctx.advance_secs(12);
        let obs_undock = fixture.call_observer("stationSvc", None, "Undock", &[], &kw).await;
        assert!(obs_undock.result.is_ok() && obs_undock.violations.is_empty());
        let obs_bind = fixture
            .call_observer(
                "beyonce",
                None,
                "MachoBindObject",
                &[PyRep::Integer(i64::from(JITA_SYS)), PyRep::None],
                &kw,
            )
            .await;
        assert!(obs_bind.result.is_ok() && obs_bind.violations.is_empty());

        ctx.advance_secs(12);
        let own_undock = fixture.call_owner("stationSvc", None, "Undock", &[], &kw).await;
        assert!(own_undock.result.is_ok() && own_undock.violations.is_empty());
        let own_bind = fixture
            .call_owner(
                "beyonce",
                None,
                "MachoBindObject",
                &[PyRep::Integer(i64::from(JITA_SYS)), PyRep::None],
                &kw,
            )
            .await;
        assert!(own_bind.violations.is_empty());
        let owner_beyonce = extract_bound_id_from_wire_rep(&own_bind.result.unwrap())
            .expect("owner Jita beyonce bound id");

        sys_jita.step_manual(1.0).await.unwrap();
        let _ = fixture.drain_pending();

        let follow_out = fixture
            .call_owner(
                "beyonce",
                Some(owner_beyonce),
                "CmdFollowBall",
                &[PyRep::Integer(obs_ship), PyRep::Real(0.0)],
                &kw,
            )
            .await;
        assert!(follow_out.result.is_ok() && follow_out.violations.is_empty());
        sys_jita.step_manual(1.0).await.unwrap();
        let _ = fixture.drain_pending();

        let orbit_out = fixture
            .call_owner(
                "beyonce",
                Some(owner_beyonce),
                "CmdOrbit",
                &[PyRep::Integer(obs_ship), PyRep::Real(2_500.0)],
                &kw,
            )
            .await;
        assert!(orbit_out.result.is_ok() && orbit_out.violations.is_empty());
        sys_jita.step_manual(1.0).await.unwrap();
        let _ = fixture.drain_pending();

        let stop_out = fixture
            .call_owner("beyonce", Some(owner_beyonce), "CmdStop", &[], &kw)
            .await;
        assert!(stop_out.result.is_ok() && stop_out.violations.is_empty());
        sys_jita.step_manual(1.0).await.unwrap();
        let _ = fixture.drain_pending();

        assert!(
            fixture
                .records_for(fixture.observer_session_id)
                .iter()
                .any(|r| r.entity_id == Some(owner_ship)),
            "observer must record owner_ship destiny events after Undock"
        );
        fixture.assert_monotonic_stamps();
        assert_clean_observability(&ctx, "Journey 1 Jita undock+nav");
    }

    // Journey 2: Maurasi (`30_000_140`) CmdWarpToStuff to `group_id == 9` asteroid belt with watcher
    {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let owner = gm_pilot_in_space(&ctx, 89_002_001, 89_002_001, 587, MAURASI_SYS).await;
        let obs = gm_pilot_in_space(&ctx, 89_002_002, 89_002_002, 587, MAURASI_SYS).await;
        let belt_id = ctx
            .sde
            .celestials_in_system_slice(MAURASI_SYS)
            .iter()
            .find(|c| c.group_id == 9)
            .expect("Maurasi must have asteroid belt group_id == 9")
            .celestial_id;
        let sys_maurasi = ctx.systems.get_or_spawn(MAURASI_SYS);
        prepare_ship_for_combat(&sys_maurasi, owner.ship_id, DVec3::new(10_000.0, 0.0, 0.0), None).await;
        prepare_ship_for_combat(&sys_maurasi, obs.ship_id, DVec3::new(12_000.0, 0.0, 0.0), None).await;

        let owner_beyonce = owner.beyonce;
        let mut fixture = owner_observer_fixture(&ctx, owner.sess, obs.sess);
        let _ = fixture
            .call_owner(
                "beyonce",
                None,
                "MachoBindObject",
                &[PyRep::Integer(i64::from(MAURASI_SYS)), PyRep::None],
                &kw,
            )
            .await;
        let _ = fixture
            .call_observer(
                "beyonce",
                None,
                "MachoBindObject",
                &[PyRep::Integer(i64::from(MAURASI_SYS)), PyRep::None],
                &kw,
            )
            .await;

        let warp_out = fixture
            .call_owner(
                "beyonce",
                Some(owner_beyonce),
                "CmdWarpToStuff",
                &[PyRep::String("item".into()), PyRep::Integer(belt_id)],
                &kw,
            )
            .await;
        assert!(warp_out.result.is_ok() && warp_out.violations.is_empty());
        for _ in 0..8 {
            sys_maurasi.step_manual(1.0).await.unwrap();
            let _ = fixture.drain_pending();
        }
        assert!(
            fixture
                .records_for(fixture.observer_session_id)
                .iter()
                .any(|r| r.action == "WarpTo"),
            "Maurasi observer must receive WarpTo update"
        );
        fixture.assert_monotonic_stamps();
        assert_clean_observability(&ctx, "Journey 2 Maurasi belt warp");
    }

    // Journey 3: Nullsec (`30_000_021`) missile volley (`499` + `1810`) while orbiting (`CmdOrbit`)
    {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let owner = gm_pilot_in_space(&ctx, 89_003_001, 89_003_001, 16236, NULLSEC_SYS).await;
        let obs = gm_pilot_in_space(&ctx, 89_003_002, 89_003_002, 587, NULLSEC_SYS).await;
        let victim = gm_pilot_in_space(&ctx, 89_003_003, 89_003_003, 587, NULLSEC_SYS).await;
        let sys_null = ctx.systems.get_or_spawn(NULLSEC_SYS);

        prepare_ship_for_combat(&sys_null, owner.ship_id, DVec3::new(100_000.0, 0.0, 0.0), None).await;
        prepare_ship_for_combat(&sys_null, victim.ship_id, DVec3::new(104_000.0, 0.0, 0.0), None).await;
        prepare_ship_for_combat(&sys_null, obs.ship_id, DVec3::new(100_000.0, 5_000.0, 0.0), None).await;

        with_mut_combat_runtime(&ctx, |rt| {
            rt.safety_levels.insert(owner.char_id, SafetyLevel::Red);
        });
        let lml_id = 89_003_901_i64;
        fit_module_and_charge(
            &ctx,
            owner.char_id,
            owner.ship_id,
            lml_id,
            499,
            InventoryFlag::HiSlot0,
            Some((89_003_902, 1810, 20)),
        );
        lock_target_sync(&sys_null, owner.ship_id, victim.ship_id).await;

        let owner_beyonce = owner.beyonce;
        let mut fixture = owner_observer_fixture(&ctx, owner.sess, obs.sess);
        let _ = fixture
            .call_owner(
                "beyonce",
                None,
                "MachoBindObject",
                &[PyRep::Integer(i64::from(NULLSEC_SYS)), PyRep::None],
                &kw,
            )
            .await;
        let _ = fixture
            .call_observer(
                "beyonce",
                None,
                "MachoBindObject",
                &[PyRep::Integer(i64::from(NULLSEC_SYS)), PyRep::None],
                &kw,
            )
            .await;

        let orbit_out = fixture
            .call_owner(
                "beyonce",
                Some(owner_beyonce),
                "CmdOrbit",
                &[PyRep::Integer(victim.ship_id), PyRep::Real(2_500.0)],
                &kw,
            )
            .await;
        assert!(orbit_out.result.is_ok() && orbit_out.violations.is_empty());
        sys_null.step_manual(1.0).await.unwrap();
        let _ = fixture.drain_pending();

        let fire_out = fixture
            .call_owner(
                "dogmaIM",
                None,
                "Activate",
                &[
                    PyRep::Integer(lml_id),
                    PyRep::String("useMissiles".into()),
                    PyRep::Integer(victim.ship_id),
                    PyRep::Integer(1),
                ],
                &kw,
            )
            .await;
        assert!(fire_out.violations.is_empty(), "missile violations: {:?}", fire_out.violations);
        let fire_res = fire_out.result.expect("useMissiles Activate");
        assert_eq!(
            fire_res.dict_get("active").and_then(|v| v.as_bool()),
            Some(true),
            "expected active missile launcher response, got {fire_res:?}"
        );
        for _ in 0..4 {
            sys_null.step_manual(1.0).await.unwrap();
            let _ = fixture.drain_pending();
        }
        assert!(
            fixture
                .records_for(fixture.observer_session_id)
                .iter()
                .any(|r| r.action == "AddBalls2" && r.entity_id.is_some_and(|id| id >= 9_950_000_000)),
            "observer must receive guided missile AddBalls2 during orbit volley"
        );
        fixture.assert_monotonic_stamps();
        assert_clean_observability(&ctx, "Journey 3 nullsec missile orbit");
    }

    // Journey 4: Nullsec (`30_000_021`) turret kill (`3041` + `246`) -> wreck -> pod via `dogmaIM.Activate`
    {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let owner = gm_pilot_in_space(&ctx, 89_004_001, 89_004_001, 16236, NULLSEC_SYS).await;
        let obs = gm_pilot_in_space(&ctx, 89_004_002, 89_004_002, 587, NULLSEC_SYS).await;
        let victim = gm_pilot_in_space(&ctx, 89_004_003, 89_004_003, 587, NULLSEC_SYS).await;
        let sys_null = ctx.systems.get_or_spawn(NULLSEC_SYS);

        prepare_ship_for_combat(&sys_null, owner.ship_id, DVec3::new(100_000.0, 0.0, 0.0), None).await;
        prepare_ship_for_combat(
            &sys_null,
            victim.ship_id,
            DVec3::new(102_000.0, 0.0, 0.0),
            Some(1.0),
        )
        .await;
        prepare_ship_for_combat(&sys_null, obs.ship_id, DVec3::new(100_000.0, 4_000.0, 0.0), None).await;

        with_mut_combat_runtime(&ctx, |rt| {
            rt.safety_levels.insert(owner.char_id, SafetyLevel::Red);
        });
        let laser_id = 89_004_901_i64;
        fit_module_and_charge(
            &ctx,
            owner.char_id,
            owner.ship_id,
            laser_id,
            3041,
            InventoryFlag::HiSlot0,
            Some((89_004_902, 246, 1)),
        );
        lock_target_sync(&sys_null, owner.ship_id, victim.ship_id).await;

        let mut fixture = owner_observer_fixture(&ctx, owner.sess, obs.sess);
        let _ = fixture
            .call_owner(
                "beyonce",
                None,
                "MachoBindObject",
                &[PyRep::Integer(i64::from(NULLSEC_SYS)), PyRep::None],
                &kw,
            )
            .await;
        let _ = fixture
            .call_observer(
                "beyonce",
                None,
                "MachoBindObject",
                &[PyRep::Integer(i64::from(NULLSEC_SYS)), PyRep::None],
                &kw,
            )
            .await;

        let kill_out = fixture
            .call_owner(
                "dogmaIM",
                None,
                "Activate",
                &[
                    PyRep::Integer(laser_id),
                    PyRep::String("targetAttack".into()),
                    PyRep::Integer(victim.ship_id),
                    PyRep::Integer(1),
                ],
                &kw,
            )
            .await;
        assert!(kill_out.result.is_ok() && kill_out.violations.is_empty());
        for _ in 0..3 {
            sys_null.step_manual(1.0).await.unwrap();
            let _ = fixture.drain_pending();
        }
        assert!(
            sys_null.get_ball_snapshot(victim.ship_id).await.unwrap().is_none(),
            "victim ship ball must be removed after lethal volley"
        );
        assert!(
            fixture
                .records_for(fixture.observer_session_id)
                .iter()
                .any(|r| {
                    matches!(r.action.as_str(), "RemoveBall" | "RemoveBalls")
                        && r.entity_id == Some(victim.ship_id)
                }),
            "observer must see RemoveBall/RemoveBalls for destroyed victim ship"
        );
        fixture.assert_monotonic_stamps();
        assert_clean_observability(&ctx, "Journey 4 nullsec laser kill->wreck->pod");
    }

    // Journey 5: Jita -> Perimeter `CmdStargateJump` under TiDi (`0.50`)
    {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let owner = gm_pilot_in_space(&ctx, 89_005_001, 89_005_001, 587, JITA_SYS).await;
        let obs = gm_pilot_in_space(&ctx, 89_005_002, 89_005_002, 587, JITA_SYS).await;
        let jita_gate = ctx
            .sde
            .jumps
            .values()
            .find(|j| j.from_solar_system_id == JITA_SYS && j.to_solar_system_id == PERIMETER_SYS)
            .expect("Jita -> Perimeter stargate")
            .clone();

        let sys_jita = ctx.systems.get_or_spawn(JITA_SYS);
        prepare_ship_for_combat(
            &sys_jita,
            owner.ship_id,
            jita_gate.position + DVec3::new(500.0, 0.0, 0.0),
            None,
        )
        .await;
        prepare_ship_for_combat(
            &sys_jita,
            obs.ship_id,
            jita_gate.position + DVec3::new(1_200.0, 0.0, 0.0),
            None,
        )
        .await;

        let owner_beyonce = owner.beyonce;
        let owner_ship = owner.ship_id;
        let mut fixture = owner_observer_fixture(&ctx, owner.sess, obs.sess);
        let _ = fixture
            .call_owner(
                "beyonce",
                None,
                "MachoBindObject",
                &[PyRep::Integer(i64::from(JITA_SYS)), PyRep::None],
                &kw,
            )
            .await;
        let _ = fixture
            .call_observer(
                "beyonce",
                None,
                "MachoBindObject",
                &[PyRep::Integer(i64::from(JITA_SYS)), PyRep::None],
                &kw,
            )
            .await;

        let tidi_jita = set_system_tidi_factor(&ctx, JITA_SYS, 0.50);
        let tidi_perim = set_system_tidi_factor(&ctx, PERIMETER_SYS, 0.50);
        assert!((tidi_jita - 0.50).abs() < 1e-6 && (tidi_perim - 0.50).abs() < 1e-6);
        let _ = fixture.drain_pending();
        ctx.advance_secs(31);

        let jump_out = fixture
            .call_owner(
                "beyonce",
                Some(owner_beyonce),
                "CmdStargateJump",
                &[
                    PyRep::Integer(jita_gate.stargate_id),
                    PyRep::Integer(jita_gate.destination_stargate_id),
                    PyRep::Integer(owner_ship),
                ],
                &kw,
            )
            .await;
        assert!(jump_out.result.is_ok() && jump_out.violations.is_empty());

        sys_jita.step_manual(1.0).await.unwrap();
        let _ = fixture.drain_pending();

        let rebind_out = fixture
            .call_owner(
                "beyonce",
                None,
                "MachoBindObject",
                &[PyRep::Integer(i64::from(PERIMETER_SYS)), PyRep::None],
                &kw,
            )
            .await;
        assert!(rebind_out.result.is_ok() && rebind_out.violations.is_empty());

        let sys_perim = ctx.systems.get_or_spawn(PERIMETER_SYS);
        sys_perim.step_manual(1.0).await.unwrap();
        let _ = fixture.drain_pending();

        assert!(
            fixture
                .records_for(fixture.observer_session_id)
                .iter()
                .any(|r| {
                    matches!(r.action.as_str(), "RemoveBall" | "RemoveBalls")
                        && r.entity_id == Some(owner_ship)
                }),
            "Jita observer must observe ownership departure RemoveBall/RemoveBalls"
        );
        fixture.assert_monotonic_stamps();
        clear_manual_system_tidi_override(&ctx, JITA_SYS);
        clear_manual_system_tidi_override(&ctx, PERIMETER_SYS);
        assert_clean_observability(&ctx, "Journey 5 Jita->Perimeter TiDi jump");
    }
}
