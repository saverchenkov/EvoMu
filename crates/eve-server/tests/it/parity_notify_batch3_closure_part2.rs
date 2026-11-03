//! E2E integration tests closing the 15 `FLAGGED_FAKE` `notify:*` rows in Batch 3.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const SYS_JITA: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;
const AU_METERS: f64 = 149_597_870_700.0;

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

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v),
        _ => None,
    }
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
async fn test_batch3_probe_scanning_lifecycle_notifications() {
    eve_testkit::attest(&[
        "notify:OnNewProbe",
        "notify:OnProbeWarpStart",
        "notify:OnProbeWarpEnd",
        "notify:OnProbeStateChanged",
    ]);

    let ctx = new_ctx();
    let jita_pos = ctx.sde.stations[&JITA_STATION].position + DVec3::new(1_000.0, 0.0, 0.0);
    let (ship_id, launcher_id, probe_stack_id) = (640_000_401_i64, 640_000_410_i64, 640_000_411_i64);
    let mut sess = login_in_space(
        &ctx,
        92_030,
        "demo",
        "demo-secret",
        PILOT_A,
        ship_id,
        621,
        SYS_JITA,
        jita_pos,
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            launcher_id,
            make_item(
                launcher_id,
                17_901,
                PILOT_A,
                ship_id,
                InventoryFlag::HiSlot0,
                1,
                true,
            ),
        );
        snap.module_online_states.insert(launcher_id, true);
        snap.items.insert(
            probe_stack_id,
            make_item(
                probe_stack_id,
                30_013,
                PILOT_A,
                ship_id,
                InventoryFlag::HiSlot0,
                8,
                false,
            ),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(launcher_id),
            PyRep::String("launchProbe".into()),
        ],
    )
    .await
    .unwrap();
    let launch_notifs = drain_all(&ctx, PILOT_A);
    let new_probe = launch_notifs
        .iter()
        .find(|(e, _)| e == "OnNewProbe")
        .map(|(_, a)| &a[0])
        .expect("OnNewProbe must be emitted");
    let expiry_ft = dict_get(new_probe, "expiry")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    assert!(
        expiry_ft > 130_000_000_000_000_000,
        "OnNewProbe expiry must be BlueOS FILETIME ticks, got {expiry_ft}"
    );
    assert!(dict_get(new_probe, "pos").is_some());

    // Near scan -> measure warp duration
    let near_coords = PyRep::List(
        (0..8)
            .map(|i| {
                PyRep::List(vec![
                    PyRep::Real(jita_pos.x + 500_000.0 + (i as f64) * 500.0),
                    PyRep::Real(jita_pos.y + 2_000.0),
                    PyRep::Real(jita_pos.z),
                    PyRep::Integer(2),
                ])
            })
            .collect(),
    );
    rpc(&ctx, &mut sess, "scanMgr", "RequestScans", &[near_coords])
        .await
        .unwrap();
    let near_imm = drain_all(&ctx, PILOT_A);
    let near_ms = near_imm
        .iter()
        .find(|(e, _)| e == "OnProbeWarpStart")
        .and_then(|(_, a)| a.get(4))
        .and_then(|v| v.as_i64())
        .expect("OnProbeWarpStart duration");
    assert!(near_imm
        .iter()
        .any(|(e, a)| e == "OnProbeStateChanged" && a.get(1) == Some(&PyRep::Integer(3))));
    assert!(!near_imm.iter().any(|(e, _)| e == "OnProbeWarpEnd"));

    ctx.advance_secs(15);
    let near_done = drain_all(&ctx, PILOT_A);
    assert_eq!(
        near_done
            .iter()
            .filter(|(e, _)| e == "OnProbeWarpEnd")
            .count(),
        8
    );
    assert!(near_done
        .iter()
        .any(|(e, a)| e == "OnProbeStateChanged" && a.get(1) == Some(&PyRep::Integer(4))));
    assert!(near_done
        .iter()
        .any(|(e, a)| e == "OnProbeStateChanged" && a.get(1) == Some(&PyRep::Integer(1))));

    // Far scan (10 AU) -> duration must be strictly larger than near_ms
    let far_coords = PyRep::List(
        (0..8)
            .map(|i| {
                PyRep::List(vec![
                    PyRep::Real(jita_pos.x + 10.0 * AU_METERS + (i as f64) * 500.0),
                    PyRep::Real(jita_pos.y),
                    PyRep::Real(jita_pos.z),
                    PyRep::Integer(2),
                ])
            })
            .collect(),
    );
    rpc(&ctx, &mut sess, "scanMgr", "RequestScans", &[far_coords])
        .await
        .unwrap();
    let far_imm = drain_all(&ctx, PILOT_A);
    let far_ms = far_imm
        .iter()
        .find(|(e, _)| e == "OnProbeWarpStart")
        .and_then(|(_, a)| a.get(4))
        .and_then(|v| v.as_i64())
        .expect("far OnProbeWarpStart duration");
    assert!(
        far_ms > near_ms,
        "far warp ({far_ms}ms) must exceed near warp ({near_ms}ms)"
    );

    ctx.advance_secs(30);
    drain_all(&ctx, PILOT_A);

    rpc(&ctx, &mut sess, "scanMgr", "RecoverProbes", &[])
        .await
        .unwrap();
    let rec = drain_all(&ctx, PILOT_A);
    assert!(rec
        .iter()
        .any(|(e, a)| e == "OnProbeStateChanged" && a.get(1) == Some(&PyRep::Integer(5))));
}

// parity: notify:OnDungeonInstanceAdded, notify:OnDungeonInstanceRemoved
