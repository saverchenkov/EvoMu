#![forbid(unsafe_code)]
#[allow(unused_imports)]
use super::*;

pub(crate) mod parity_notify_batch2 {
    #[allow(unused_imports)]
    use super::*;

// End-to-end integration tests for remediated `notify:*` parity rows:
// - `notify:DoSimClockRebase`
// - `notify:OnDisapprovalTimerUpdate`
// - `notify:OnInvalidateCacheForLocations`

use super::super::dungeon_escalation_and_fw_bunker_notices::parity_notify_batch2::fit_guns_and_skills;
use crate::common;
use crate::notify_structures_clones_pi::{login_in_space, make_item};
use eve_db::InventoryFlag;
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;
const SYS_AMARR: u32 = 30_002_187;
const SYS_ZARZAKH: u32 = 30_100_000;
const STATION_JITA: i64 = 60_003_760;
fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    eve_server::services::serverops_cluster::set_scheduled_downtime_enabled(&ctx, false);
    ctx
}

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

#[tokio::test]
async fn test_batch2_sim_clock_rebase_disapproval_and_cache_invalidation() {
    // parity: notify:DoSimClockRebase
    eve_testkit::attest(&[
        "notify:DoSimClockRebase",
    ]);

    let ctx = new_ctx();

    // 1. DoSimClockRebase via GM /tidi slash command in Jita space
    let mut sess_jita = login_in_space(
        &ctx, 92_001, "demo", "demo-secret", PILOT_A, 720_000_001, 587, SYS_JITA, DVec3::ZERO,
    ).await;
    sess_jita.role |= ROLE_GM_DEFAULT;
    let _sess_amarr = login_in_space(
        &ctx, 92_002, "other", "other-secret", PILOT_B, 720_000_002, 587, SYS_AMARR, DVec3::ZERO,
    ).await;
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    rpc(&ctx, &mut sess_jita, "slash", "SlashCmd", &[PyRep::String("/tidi 0.50".into())])
        .await
        .expect("/tidi 0.50");

    let jita_notifs = ctx.drain_notifications_for_char(PILOT_A);
    let rebase = jita_notifs
        .iter()
        .find(|n| n.event == "DoSimClockRebase")
        .expect("DoSimClockRebase emitted to Jita pilot");
    assert_eq!(rebase.args.len(), 1);
    let PyRep::Tuple(pair) = &rebase.args[0] else {
        panic!("expected 2-tuple in DoSimClockRebase, got {:?}", rebase.args);
    };
    assert_eq!(pair.len(), 2);
    assert!(pair[0].as_i64().expect("oldSimTime") < pair[1].as_i64().expect("newSimTime"));
    assert!(
        ctx.drain_notifications_for_char(PILOT_B).iter().all(|n| n.event != "DoSimClockRebase"),
        "Amarr pilot must not receive Jita DoSimClockRebase"
    );

    // 2. OnDisapprovalTimerUpdate via live weapon aggression in Zarzakh + cluster tick
    let (ship_z1, ship_z2, gun_z) = (720_000_101_i64, 720_000_102_i64, 720_000_110_i64);
    let mut sess_z1 = login_in_space(
        &ctx, 92_003, "demo", "demo-secret", PILOT_A, ship_z1, 587, SYS_ZARZAKH, DVec3::ZERO,
    ).await;
    let _sess_z2 = login_in_space(
        &ctx, 92_004, "other", "other-secret", PILOT_B, ship_z2, 587, SYS_ZARZAKH, DVec3::new(1_000.0, 0.0, 0.0),
    ).await;
    fit_guns_and_skills(&ctx, PILOT_A, ship_z1, gun_z, 3074, 12_625);
    let sys_z = ctx.systems.get_or_spawn(SYS_ZARZAKH);
    rpc(&ctx, &mut sess_z1, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_z2)]).await.expect("AddTarget");
    sys_z.step_manual(5.0).await.unwrap();
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    rpc(
        &ctx, &mut sess_z1, "dogmaIM", "Activate",
        &[PyRep::Integer(gun_z), PyRep::String("projectileFired".into()), PyRep::Integer(ship_z2)],
    ).await.expect("Activate weapon in Zarzakh");

    let dis_active = ctx.drain_notifications_for_char(PILOT_A)
        .into_iter()
        .rfind(|n| n.event == "OnDisapprovalTimerUpdate")
        .expect("OnDisapprovalTimerUpdate active (501)");
    assert_eq!(dis_active.args.len(), 2);
    assert_eq!(dis_active.args[0], PyRep::Integer(501));
    assert!(dis_active.args[1].as_i64().unwrap_or(0) > 0);

    // Advance 2s + cluster tick -> countdown state (502)
    ctx.advance_secs(2);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let dis_cd = ctx.drain_notifications_for_char(PILOT_A)
        .into_iter()
        .find(|n| n.event == "OnDisapprovalTimerUpdate")
        .expect("OnDisapprovalTimerUpdate countdown (502)");
    assert_eq!(dis_cd.args[0], PyRep::Integer(502));
    assert!(dis_cd.args[1].as_i64().unwrap_or(0) > 0);

    // Advance past 900s expiry + cluster tick -> idle state (500, None)
    ctx.advance_secs(905);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let dis_idle = ctx.drain_notifications_for_char(PILOT_A)
        .into_iter()
        .find(|n| n.event == "OnDisapprovalTimerUpdate")
        .expect("OnDisapprovalTimerUpdate idle (500)");
    assert_eq!(dis_idle.args, vec![PyRep::Integer(500), PyRep::None]);

    // 3. OnInvalidateCacheForLocations via bound invbroker.SetPassword
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(STATION_JITA);
            ch.solar_system_id = SYS_JITA;
        }
    });
    let mut sess_st = SessionState::new(92_005);
    common::login_pilot(&ctx, &mut sess_st, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess_st, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)]).await.expect("SelectCharacterID");
    sess_st.station_id = Some(STATION_JITA);
    sess_st.solar_system_id = None;
    ctx.sync_session(&mut sess_st);
    let container_id: i64 = 720_300_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            container_id,
            make_item(container_id, 3465, PILOT_A, STATION_JITA, InventoryFlag::Hangar, 1, true),
        );
    });
    let bind = rpc(
        &ctx, &mut sess_st, "invbroker", "MachoBindObject",
        &[PyRep::Tuple(vec![PyRep::Integer(STATION_JITA), PyRep::Integer(15)]), PyRep::None],
    ).await.expect("MachoBindObject");
    let mgr_oid = match &bind {
        PyRep::Tuple(parts) => parts[0].as_i64().expect("mgr_oid") as u64,
        _ => panic!("expected tuple"),
    };
    let cont_oid = ctx.dispatch_rpc(
        &mut sess_st, "invbroker", Some(mgr_oid), "GetInventoryFromId",
        &[PyRep::Integer(container_id), PyRep::Integer(0)], &HashMap::new(), None,
    ).await.expect("GetInventoryFromId").as_i64().expect("cont_oid") as u64;
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    ctx.dispatch_rpc(
        &mut sess_st, "invbroker", Some(cont_oid), "SetPassword",
        &[PyRep::Integer(2), PyRep::String("alpha123".into()), PyRep::String(String::new())],
        &HashMap::new(), None,
    ).await.expect("SetPassword");

    let inv = ctx.drain_notifications_for_char(PILOT_A)
        .into_iter()
        .find(|n| n.event == "OnInvalidateCacheForLocations")
        .expect("OnInvalidateCacheForLocations");
    assert_eq!(inv.args, vec![PyRep::List(vec![PyRep::Integer(container_id)])]);
}

}
