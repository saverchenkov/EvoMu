//! End-to-end integration tests for [`eve_testkit::shadow_client::ShadowClient`] replica convergence
//! (`#930`, RFC-3 `§3` & `§5 G4`, `#937`, `#941`, `#948`).

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_testkit::shadow_client::{ShadowBallMode, ShadowClient, ShadowDivergence};
use glam::DVec3;
use std::collections::HashMap;

const PILOT: i64 = 90_000_001;
const JITA: u32 = 30_000_142;

fn drain_and_apply_notifications(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    shadow: &mut ShadowClient,
) {
    let _ = sess.drain_notifications();
    for notif in ctx.drain_notifications_for_char(char_id) {
        shadow.apply_notification(&notif.event, &notif.args);
    }
}

async fn login_and_select_pilot(ctx: &ServerContext, sess_id: u64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
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
    let _ = sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(PILOT);
    sess
}

#[tokio::test]
async fn test_shadow_client_catches_missing_deltas_and_converges_on_live_rpcs() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.get_mut("demo") {
            acc.role |= eve_db::service_roles::ROLE_GM_DEFAULT;
        }
    });
    let mut sess = login_and_select_pilot(&ctx, 930_001).await;

    let mut shadow = ShadowClient::from_world(PILOT, &ctx.db, Some(&ctx.sde));
    shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

    // 1. Silent world inventory mutation (no OnItemChange emitted) is immediately caught by ShadowClient.
    let silent_item_id = 990_930_001;
    let station_id = sess.station_id.expect("docked in Jita 4-4");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            silent_item_id,
            InventoryItemRecord {
                item_id: silent_item_id,
                type_id: 34, // Tritanium
                owner_id: PILOT,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let inv_divs = shadow.compare_inventory_against_world(&ctx.db, Some(&ctx.sde));
    assert!(
        inv_divs.iter().any(|d| matches!(
            d,
            ShadowDivergence::MissingShadowItem {
                item_id,
                type_id: 34,
                quantity: 1_000,
                ..
            } if *item_id == silent_item_id
        )),
        "ShadowClient must detect silent inventory mutation without OnItemChange, got {inv_divs:?}"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&silent_item_id);
    });
    shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

    // 2. Silent world wallet mutation (no OnAccountChange emitted) is immediately caught by ShadowClient.
    let old_balance = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&PILOT).unwrap().balance);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&PILOT).unwrap().balance =
            old_balance.checked_add(IskCents::from_isk(777_000)).unwrap();
    });
    let wallet_divs = shadow.compare_wallet_against_world(&ctx.db);
    assert!(
        wallet_divs.iter().any(|d| matches!(
            d,
            ShadowDivergence::PersonalWalletMismatch { char_id: PILOT, .. }
        )),
        "ShadowClient must detect silent wallet mutation without OnAccountChange, got {wallet_divs:?}"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&PILOT).unwrap().balance = old_balance;
    });
    shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

    // 3. Live RPC mutations (/createitem and /giveisk) emit OnItemChange and OnAccountChange -> ShadowClient converges.
    common::slash_cmd(&ctx, &mut sess, "/createitem 34 250").await;
    common::slash_cmd(&ctx, &mut sess, "/giveisk 500000").await;
    drain_and_apply_notifications(&ctx, &mut sess, PILOT, &mut shadow);
    shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));
}

#[tokio::test]
async fn test_shadow_client_941_948_loaded_charges_across_login_undock_jump_and_unload() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = login_and_select_pilot(&ctx, 930_002).await;
    let ship_id = sess.ship_id.expect("active ship");
    let gun_id = 990_930_101;
    let ammo_stack_id = 990_930_102;
    let probe_launcher_id = 990_930_103;
    let probe_stack_id = 990_930_104;

    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(PILOT).or_default();
        for sid in [3300u32, 3301, 3402, 3412, 3413, 3426] {
            sk.insert(
                sid,
                TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        snap.items.insert(
            gun_id,
            InventoryItemRecord {
                item_id: gun_id,
                type_id: 484, // 125mm Gatling AutoCannon I
                owner_id: PILOT,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ammo_stack_id,
            InventoryItemRecord {
                item_id: ammo_stack_id,
                type_id: 179, // Depleted Uranium S
                owner_id: PILOT,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 200,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            probe_launcher_id,
            InventoryItemRecord {
                item_id: probe_launcher_id,
                type_id: 17938, // Core Probe Launcher I
                owner_id: PILOT,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            probe_stack_id,
            InventoryItemRecord {
                item_id: probe_stack_id,
                type_id: 30013, // Core Scanner Probe I
                owner_id: PILOT,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 8,
                singleton: false,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(gun_id, true);
        snap.module_online_states.insert(probe_launcher_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let mut shadow = ShadowClient::from_world(PILOT, &ctx.db, Some(&ctx.sde));
    shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

    // 1. Load ammo into HiSlot0 and scanner probes into HiSlot1 while docked via dogmaIM.LoadAmmo.
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(gun_id)]),
            PyRep::List(vec![PyRep::Integer(ammo_stack_id)]),
            PyRep::Integer(ship_id),
        ],
        &kw,
        None,
    )
    .await
    .expect("dogmaIM.LoadAmmo autocannon");

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(probe_launcher_id)]),
            PyRep::List(vec![PyRep::Integer(probe_stack_id)]),
            PyRep::Integer(ship_id),
        ],
        &kw,
        None,
    )
    .await
    .expect("dogmaIM.LoadAmmo probe launcher");

    drain_and_apply_notifications(&ctx, &mut sess, PILOT, &mut shadow);
    shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

    // 2. Call dogmaIM.GetAllInfo while docked (login prime) -> verify loaded charges in HiSlot0 & HiSlot1 converge.
    let all_info_login = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "GetAllInfo",
            &[PyRep::Bool(true), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("dogmaIM.GetAllInfo on login");
    drain_and_apply_notifications(&ctx, &mut sess, PILOT, &mut shadow);
    shadow.sync_from_get_all_info(&all_info_login);
    shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));
    assert!(shadow
        .inventory
        .sublocations_by_ship_flag
        .contains_key(&(ship_id, InventoryFlag::HiSlot0 as u16)));
    assert!(shadow
        .inventory
        .sublocations_by_ship_flag
        .contains_key(&(ship_id, InventoryFlag::HiSlot1 as u16)));

    // 3. Undock (stationSvc.Undock) and call dogmaIM.GetAllInfo (#948) -> verify convergence after undock.
    ctx.advance_secs(12);
    let undock_res = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("stationSvc.Undock");
    let beyonce_id = undock_res
        .dict_get("beyonceBoundID")
        .and_then(|v| v.as_i64())
        .expect("beyonceBoundID") as u64;
    drain_and_apply_notifications(&ctx, &mut sess, PILOT, &mut shadow);

    let all_info_undock = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "GetAllInfo",
            &[PyRep::Bool(true), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("dogmaIM.GetAllInfo after undock");
    drain_and_apply_notifications(&ctx, &mut sess, PILOT, &mut shadow);
    shadow.sync_from_get_all_info(&all_info_undock);
    shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

    // 4. Stargate jump (beyonce.CmdStargateJump) and call dogmaIM.GetAllInfo (#941, #948) -> verify convergence after jump.
    let jita_handle = ctx.systems.get_or_spawn(JITA);
    let jita_gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == JITA)
        .cloned()
        .expect("Jita stargate");
    jita_handle
        .set_ball_position(ship_id, jita_gate.position + DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    ctx.advance_secs(12);
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdStargateJump",
        &[PyRep::Integer(jita_gate.stargate_id)],
        &kw,
        None,
    )
    .await
    .expect("beyonce.CmdStargateJump");
    drain_and_apply_notifications(&ctx, &mut sess, PILOT, &mut shadow);

    let all_info_jump = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "GetAllInfo",
            &[PyRep::Bool(true), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("dogmaIM.GetAllInfo after stargate jump");
    drain_and_apply_notifications(&ctx, &mut sess, PILOT, &mut shadow);
    shadow.sync_from_get_all_info(&all_info_jump);
    shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

    // 5. Unload ammo in space via dogmaIM.UnloadAmmo (#941) -> no UnprimedFittedItemChange and full convergence.
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "UnloadAmmo",
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(gun_id)]),
        ],
        &kw,
        None,
    )
    .await
    .expect("dogmaIM.UnloadAmmo in space");
    drain_and_apply_notifications(&ctx, &mut sess, PILOT, &mut shadow);
    shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));
    assert!(!shadow
        .inventory
        .sublocations_by_ship_flag
        .contains_key(&(ship_id, InventoryFlag::HiSlot0 as u16)));

    // 6. Activate one-shot scan probe launcher (#937) -> start=1 and stop=0 OnGodmaShipEffect converge active_modules.
    let dest_sys_id = sess.solar_system_id.expect("in destination system");
    let dest_handle = ctx.systems.get_or_spawn(dest_sys_id);
    if let Ok((_, _, init_updates)) = dest_handle.drain_observer_updates(ship_id).await {
        shadow.apply_notification("DoDestinyUpdate", &[init_updates]);
    }

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(probe_launcher_id),
            PyRep::String("useMissiles".into()),
            PyRep::None,
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("dogmaIM.Activate Core Probe Launcher I (#937)");

    drain_and_apply_notifications(&ctx, &mut sess, PILOT, &mut shadow);
    if let Ok((_, _, probe_updates)) = dest_handle.drain_observer_updates(ship_id).await {
        shadow.apply_notification("DoDestinyUpdate", &[probe_updates]);
    }

    shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));
    assert!(
        shadow.module_states.active_modules.is_empty(),
        "one-shot probe launcher must not remain stuck active in ShadowClient (#937)"
    );
    assert!(
        !shadow
            .inventory
            .sublocations_by_ship_flag
            .contains_key(&(ship_id, InventoryFlag::HiSlot1 as u16)),
        "expended probe sublocation must be cleared after launching all 8 probes"
    );
    for probe_rec in ctx.active_scanner_probes.read().unwrap().values() {
        if probe_rec.owner_char_id == PILOT && probe_rec.solar_system_id == dest_sys_id {
            let sh_ball = shadow
                .ballpark
                .balls
                .get(&probe_rec.probe_id)
                .expect("launched scanner probe ball must be present in ShadowBallpark");
            assert_eq!(sh_ball.mode, ShadowBallMode::Stop);
        }
    }
}
