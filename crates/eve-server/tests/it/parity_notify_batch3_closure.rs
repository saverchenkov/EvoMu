//! E2E integration tests closing the 15 `FLAGGED_FAKE` `notify:*` rows in Batch 3.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;
const SYS_AMARR: u32 = 30_002_187;
const SYS_ZARZAKH: u32 = 30_100_000;
const JITA_STATION: i64 = 60_003_760;

fn new_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn drain_all(ctx: &ServerContext, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    ctx.drain_notifications_for_char(char_id)
        .into_iter()
        .map(|n| (n.event, n.args))
        .collect()
}

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
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

async fn login_in_space(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(
                ship_id,
                ship_type_id,
                char_id,
                i64::from(sys_id),
                InventoryFlag::None,
                1,
                true,
            ),
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
            eve_destiny::SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: char_id,
                char_id: Some(char_id),
                corp_id: 1000009,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    sess
}

// parity: notify:DoSimClockRebase, notify:OnDisapprovalTimerUpdate, notify:OnNpcTimerUpdate
#[tokio::test]
async fn test_batch3_sim_clock_rebase_and_crimewatch_timers() {
    eve_testkit::attest(&[
        "notify:DoSimClockRebase",
        "notify:OnDisapprovalTimerUpdate",
        "notify:OnNpcTimerUpdate",
    ]);

    let ctx = new_ctx();
    let mut gm_sess = login_in_space(
        &ctx,
        92_001,
        "demo",
        "demo-secret",
        PILOT_A,
        640_000_001,
        587,
        SYS_JITA,
        DVec3::ZERO,
    )
    .await;
    gm_sess.role = eve_db::service_roles::ROLE_GM_DEFAULT;
    let _amarr_sess = login_in_space(
        &ctx,
        92_002,
        "other",
        "other-secret",
        PILOT_B,
        640_000_002,
        587,
        SYS_AMARR,
        DVec3::ZERO,
    )
    .await;
    drain_all(&ctx, PILOT_A);
    drain_all(&ctx, PILOT_B);

    rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/tidi 0.5".into())],
    )
    .await
    .unwrap();
    let jita_notifs = drain_all(&ctx, PILOT_A);
    let amarr_notifs = drain_all(&ctx, PILOT_B);
    let rebase = jita_notifs
        .iter()
        .find(|(e, _)| e == "DoSimClockRebase")
        .expect("Jita pilot must receive DoSimClockRebase");
    let PyRep::Tuple(pair) = &rebase.1[0] else {
        panic!("expected 2-tuple in DoSimClockRebase");
    };
    assert_eq!(pair.len(), 2);
    assert!(pair[0].as_i64().unwrap() < pair[1].as_i64().unwrap());
    assert!(
        !amarr_notifs.iter().any(|(e, _)| e == "DoSimClockRebase"),
        "Amarr pilot must not receive Jita DoSimClockRebase"
    );

    // Zarzakh disapproval timer (501 active -> 502 countdown -> 500 idle) & NPC timer (401 -> 402 -> 400)
    let (ship_a, ship_b, gun_id) = (640_000_101_i64, 640_000_102_i64, 640_000_110_i64);
    let mut s_a = login_in_space(
        &ctx,
        92_003,
        "demo",
        "demo-secret",
        PILOT_A,
        ship_a,
        587,
        SYS_ZARZAKH,
        DVec3::ZERO,
    )
    .await;
    let _s_b = login_in_space(
        &ctx,
        92_004,
        "other",
        "other-secret",
        PILOT_B,
        ship_b,
        587,
        SYS_ZARZAKH,
        DVec3::new(1_000.0, 0.0, 0.0),
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            gun_id,
            make_item(gun_id, 484, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true),
        );
        snap.module_online_states.insert(gun_id, true);
    });
    ctx.invalidate_ship_fitting(ship_a);
    let sys_h = ctx.systems.get_or_spawn(SYS_ZARZAKH);
    sys_h
        .add_target_lock(
            ship_a,
            TargetLock {
                target_id: ship_b,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .unwrap();
    drain_all(&ctx, PILOT_A);

    rpc(
        &ctx,
        &mut s_a,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun_id),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(ship_b),
        ],
    )
    .await
    .unwrap();
    let n1 = drain_all(&ctx, PILOT_A);
    assert!(
        n1.iter()
            .any(|(e, a)| e == "OnDisapprovalTimerUpdate" && a.first() == Some(&PyRep::Integer(501))),
        "expected OnDisapprovalTimerUpdate(501, exp), got {n1:?}"
    );

    ctx.advance_secs(1);
    let n2 = drain_all(&ctx, PILOT_A);
    assert!(
        n2.iter()
            .any(|(e, a)| e == "OnDisapprovalTimerUpdate" && a.first() == Some(&PyRep::Integer(502))),
        "expected OnDisapprovalTimerUpdate(502, exp) countdown, got {n2:?}"
    );

    let npc_id = 640_000_199_i64;
    sys_h
        .spawn_ball(Ball {
            id: npc_id,
            type_id: 16_250,
            position: DVec3::new(1_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await
        .unwrap();
    sys_h
        .add_target_lock(
            ship_a,
            TargetLock {
                target_id: npc_id,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .unwrap();
    drain_all(&ctx, PILOT_A);

    rpc(
        &ctx,
        &mut s_a,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun_id),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(npc_id),
        ],
    )
    .await
    .unwrap();
    let n3 = drain_all(&ctx, PILOT_A);
    assert!(
        n3.iter()
            .any(|(e, a)| e == "OnNpcTimerUpdate" && a.first() == Some(&PyRep::Integer(401))),
        "expected OnNpcTimerUpdate(401, exp), got {n3:?}"
    );

    ctx.advance_secs(1);
    let n4 = drain_all(&ctx, PILOT_A);
    assert!(
        n4.iter()
            .any(|(e, a)| e == "OnNpcTimerUpdate" && a.first() == Some(&PyRep::Integer(402))),
        "expected OnNpcTimerUpdate(402, exp) countdown, got {n4:?}"
    );

    ctx.advance_secs(910);
    let n5 = drain_all(&ctx, PILOT_A);
    assert!(
        n5.iter().any(|(e, a)| e == "OnNpcTimerUpdate"
            && a == &vec![PyRep::Integer(400), PyRep::None]),
        "expected OnNpcTimerUpdate(400, None) on expiry"
    );
    assert!(
        n5.iter().any(|(e, a)| e == "OnDisapprovalTimerUpdate"
            && a == &vec![PyRep::Integer(500), PyRep::None]),
        "expected OnDisapprovalTimerUpdate(500, None) on expiry"
    );
}

// parity: notify:OnPasswordChanged, notify:OnInvalidateCacheForLocations
#[tokio::test]
async fn test_batch3_container_password_and_cache_invalidation() {
    eve_testkit::attest(&[
        "notify:OnPasswordChanged",
        "notify:OnInvalidateCacheForLocations",
    ]);

    let ctx = new_ctx();
    let mut sess = SessionState::new(92_010);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(PILOT_A)],
    )
    .await
    .unwrap();

    let container_id = 640_000_250_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            container_id,
            make_item(
                container_id,
                17_366, // Audit Log Secure Container (group 448)
                PILOT_A,
                JITA_STATION,
                InventoryFlag::Hangar,
                1,
                true,
            ),
        );
    });

    let bind = rpc(
        &ctx,
        &mut sess,
        "invbroker",
        "MachoBindObject",
        &[
            PyRep::Tuple(vec![PyRep::Integer(JITA_STATION), PyRep::Integer(15)]),
            PyRep::None,
        ],
    )
    .await
    .unwrap();
    let mgr_oid = match &bind {
        PyRep::Tuple(parts) => parts[0].as_i64().unwrap() as u64,
        _ => panic!("expected MachoBindObject tuple"),
    };
    let bound_id = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(mgr_oid),
            "GetInventoryFromId",
            &[PyRep::Integer(container_id), PyRep::Integer(0)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap()
        .as_i64()
        .unwrap() as u64;
    drain_all(&ctx, PILOT_A);

    // Set general password (which = 1)
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        Some(bound_id),
        "SetPassword",
        &[
            PyRep::Integer(1),
            PyRep::String("general123".into()),
            PyRep::None,
        ],
        &HashMap::new(),
        None,
    )
    .await
    .unwrap();
    let n1 = drain_all(&ctx, PILOT_A);
    assert!(
        n1.iter()
            .any(|(e, a)| e == "OnPasswordChanged" && a.first() == Some(&PyRep::Integer(1))),
        "expected OnPasswordChanged(1, item), got {n1:?}"
    );
    assert!(
        n1.iter().any(|(e, a)| e == "OnInvalidateCacheForLocations"
            && a.first() == Some(&PyRep::List(vec![PyRep::Integer(container_id)]))),
        "expected OnInvalidateCacheForLocations([container_id]), got {n1:?}"
    );

    // Set config password (which = 2)
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        Some(bound_id),
        "SetPassword",
        &[
            PyRep::Integer(2),
            PyRep::String("config123".into()),
            PyRep::None,
        ],
        &HashMap::new(),
        None,
    )
    .await
    .unwrap();
    let n2 = drain_all(&ctx, PILOT_A);
    assert!(
        n2.iter()
            .any(|(e, a)| e == "OnPasswordChanged" && a.first() == Some(&PyRep::Integer(2))),
        "expected OnPasswordChanged(2, item), got {n2:?}"
    );
    assert!(
        n2.iter().any(|(e, a)| e == "OnInvalidateCacheForLocations"
            && a.first() == Some(&PyRep::List(vec![PyRep::Integer(container_id)]))),
        "expected OnInvalidateCacheForLocations([container_id]), got {n2:?}"
    );

    // Wrong oldPassword must fail
    let bad = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(bound_id),
            "SetPassword",
            &[
                PyRep::Integer(1),
                PyRep::String("newpass".into()),
                PyRep::String("wrong_old".into()),
            ],
            &HashMap::new(),
            None,
        )
        .await;
    assert_eq!(bad.unwrap_err().code, "InvalidPassword");
}

// parity: notify:OnMultiEvent, notify:OnKillNotification
#[tokio::test]
async fn test_batch3_target_multi_event_and_kill_notification() {
    eve_testkit::attest(&["notify:OnMultiEvent", "notify:OnKillNotification"]);

    let ctx = new_ctx();
    let (ship_a, ship_b, gun_id, ammo_id) = (640_000_301_i64, 640_000_302_i64, 640_000_310_i64, 640_000_311_i64);
    let mut sess_a = login_in_space(
        &ctx,
        92_020,
        "demo",
        "demo-secret",
        PILOT_A,
        ship_a,
        587,
        SYS_ZARZAKH,
        DVec3::ZERO,
    )
    .await;
    let _sess_b = login_in_space(
        &ctx,
        92_021,
        "other",
        "other-secret",
        PILOT_B,
        ship_b,
        587,
        SYS_ZARZAKH,
        DVec3::new(1_000.0, 0.0, 0.0),
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            gun_id,
            make_item(gun_id, 484, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true),
        );
        snap.items.insert(
            ammo_id,
            make_item(ammo_id, 218, PILOT_A, ship_a, InventoryFlag::HiSlot0, 100, false),
        );
        snap.module_online_states.insert(gun_id, true);
    });
    ctx.invalidate_ship_fitting(ship_a);
    drain_all(&ctx, PILOT_A);
    drain_all(&ctx, PILOT_B);

    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(ship_b)],
    )
    .await
    .unwrap();
    ctx.systems
        .get_or_spawn(SYS_ZARZAKH)
        .step_manual(10.0)
        .await
        .unwrap();

    let lock_a = drain_all(&ctx, PILOT_A);
    let lock_b = drain_all(&ctx, PILOT_B);
    let has_target_ev = |notifs: &[(String, Vec<PyRep>)], sub: &str, id: i64| {
        notifs.iter().any(|(e, a)| {
            e == "OnMultiEvent"
                && match a.first() {
                    Some(PyRep::List(items)) => items.iter().any(|it| match it {
                        PyRep::Tuple(outer) => match outer.first() {
                            Some(PyRep::Tuple(inner)) => {
                                inner.first() == Some(&PyRep::String("OnTarget".into()))
                                    && inner.get(1) == Some(&PyRep::String(sub.into()))
                                    && inner.get(2) == Some(&PyRep::Integer(id))
                            }
                            _ => false,
                        },
                        _ => false,
                    }),
                    _ => false,
                }
        })
    };
    assert!(
        has_target_ev(&lock_a, "add", ship_b),
        "locker must receive OnMultiEvent OnTarget add"
    );
    assert!(
        has_target_ev(&lock_b, "otheradd", ship_a),
        "target must receive OnMultiEvent OnTarget otheradd"
    );

    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "RemoveTarget",
        &[PyRep::Integer(ship_b)],
    )
    .await
    .unwrap();
    let unlock_b = drain_all(&ctx, PILOT_B);
    assert!(
        has_target_ev(&unlock_b, "otherlost", ship_a),
        "target must receive OnMultiEvent OnTarget otherlost"
    );

    // Re-lock and destroy ship_b via weapon fire
    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(ship_b)],
    )
    .await
    .unwrap();
    ctx.systems
        .get_or_spawn(SYS_ZARZAKH)
        .step_manual(10.0)
        .await
        .unwrap();
    drain_all(&ctx, PILOT_A);
    drain_all(&ctx, PILOT_B);

    let sys_h = ctx.systems.get_or_spawn(SYS_ZARZAKH);
    for _ in 0..40 {
        if sys_h.get_ball_snapshot(ship_b).await.ok().flatten().is_none() {
            break;
        }
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(gun_id),
                PyRep::String("projectileFired".into()),
                PyRep::Integer(ship_b),
            ],
        )
        .await;
        ctx.advance_secs(5);
        let _ = sys_h.step_manual(5.0).await;
    }
    assert!(
        sys_h.get_ball_snapshot(ship_b).await.ok().flatten().is_none(),
        "ship_b must be destroyed by weapon fire"
    );
    let kill_a = drain_all(&ctx, PILOT_A);
    let kill_b = drain_all(&ctx, PILOT_B);
    assert!(
        kill_a.iter().any(|(e, _)| e == "OnKillNotification"),
        "attacker must receive OnKillNotification"
    );
    assert!(
        kill_b.iter().any(|(e, _)| e == "OnKillNotification"),
        "victim must receive OnKillNotification"
    );
}

// parity: notify:OnNewProbe, notify:OnProbeWarpStart, notify:OnProbeWarpEnd, notify:OnProbeStateChanged
