#![forbid(unsafe_code)]
#[allow(unused_imports)]
use super::*;

pub mod b30 {

use crate::common;
use crate::notify_structures_clones_pi::{login_in_space, make_item};
use eve_db::{
    corp_roles::{ROLE_DIRECTOR, ROLE_EQUIPMENT_CONFIG},
    InventoryFlag,
};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const SYS_JITA: u32 = 30_000_142;
const STATION_JITA: i64 = 60_003_760;
fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
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

fn kv_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.iter().find_map(|(k, v)| if k == key { Some(v) } else { None }),
        PyRep::Object { args, .. } => kv_get(args, key),
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| match k {
            PyRep::String(s) if s == key => Some(v),
            _ => None,
        }),
        _ => None,
    }
}

// parity: notify:OnOperationReplayed, notify:OnUiRevealForced
#[tokio::test]
async fn notify_b30_operations_and_container_password() {
    eve_testkit::attest(&[
        "notify:OnOperationReplayed",
        "notify:OnUiRevealForced",
    ]);

    let ctx = new_ctx();
    let mut unauth = SessionState::new(9_600);
    assert_eq!(
        rpc(&ctx, &mut unauth, "slash", "SlashCmd", &[PyRep::String("/operation reveal_ui".into())])
            .await
            .unwrap_err()
            .code,
        "Unauthorized"
    );

    let mut sess_a = SessionState::new(9_601);
    common::login_pilot(&ctx, &mut sess_a, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess_a, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)])
        .await
        .expect("SelectCharacterID A");
    sess_a.role |= ROLE_GM_DEFAULT;
    sess_a.station_id = Some(STATION_JITA);
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    // 1. OnOperationReplayed: negative invalid op rejected without emitting
    assert!(rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String("/operation replay 0 0".into())])
        .await
        .is_err());
    assert!(ctx.drain_notifications_for_char(PILOT_A).iter().all(|n| n.event != "OnOperationReplayed"));

    // Positive: /operation replay <cat_id> <op_id> mutates CharacterOperationsRecord and emits OnOperationReplayed
    let op_id: i64 = ctx
        .sde
        .operations()
        .expect("operations")
        .keys()
        .filter_map(|k| k.parse::<i64>().ok())
        .min()
        .expect("at least one operation in SDE");
    assert_eq!(
        rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String(format!("/operation replay 1 {op_id}"))])
            .await
            .expect("SlashCmd /operation replay"),
        PyRep::Bool(true)
    );
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    let replayed = notifs.iter().find(|n| n.event == "OnOperationReplayed").expect("OnOperationReplayed");
    assert_eq!(replayed.id_type, "charid");
    assert_eq!(replayed.args, vec![PyRep::Integer(1), PyRep::Integer(op_id)]);

    // 2. OnUiRevealForced: /operation reveal_ui emits OnUiRevealForced() with 0 args
    assert_eq!(
        rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String("/operation reveal_ui".into())])
            .await
            .expect("SlashCmd /operation reveal_ui"),
        PyRep::Bool(true)
    );
    let revealed = ctx
        .drain_notifications_for_char(PILOT_A)
        .into_iter()
        .find(|n| n.event == "OnUiRevealForced")
        .expect("OnUiRevealForced");
    assert_eq!(revealed.id_type, "charid");
    assert!(revealed.args.is_empty());

    // 3 & 4. OnPasswordChanged & OnInvalidateCacheForLocations via bound invbroker.SetPassword
    let container_id: i64 = 99_300_001;
    let corp_container_id: i64 = 99_300_002;
    let container_type_id: u32 = 3465; // Small Secure Container
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            container_id,
            make_item(container_id, container_type_id, PILOT_A, STATION_JITA, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            corp_container_id,
            make_item(corp_container_id, container_type_id, 2001, STATION_JITA, InventoryFlag::Hangar, 1, true),
        );
    });
    let bind = rpc(
        &ctx,
        &mut sess_a,
        "invbroker",
        "MachoBindObject",
        &[PyRep::Tuple(vec![PyRep::Integer(STATION_JITA), PyRep::Integer(15)]), PyRep::None],
    )
    .await
    .expect("MachoBindObject");
    let mgr_oid = match &bind {
        PyRep::Tuple(parts) => parts[0].as_i64().expect("mgr bound id") as u64,
        _ => panic!("expected MachoBindObject tuple"),
    };
    let oid = ctx
        .dispatch_rpc(
            &mut sess_a,
            "invbroker",
            Some(mgr_oid),
            "GetInventoryFromId",
            &[PyRep::Integer(container_id), PyRep::Integer(0)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("GetInventoryFromId")
        .as_i64()
        .expect("container bound id") as u64;
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    // Set initial config password (which = 2) and general password (which = 1) independently
    ctx.dispatch_rpc(
        &mut sess_a,
        "invbroker",
        Some(oid),
        "SetPassword",
        &[PyRep::Integer(2), PyRep::String("alpha123".into()), PyRep::String(String::new())],
        &HashMap::new(),
        None,
    )
    .await
    .expect("bound SetPassword which=2");

    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    let pw_notif = notifs.iter().find(|n| n.event == "OnPasswordChanged").expect("OnPasswordChanged");
    assert_eq!(pw_notif.args.len(), 2);
    assert_eq!(pw_notif.args[0], PyRep::Integer(2));
    assert_eq!(kv_get(&pw_notif.args[1], "itemID"), Some(&PyRep::Integer(container_id)));
    assert_eq!(kv_get(&pw_notif.args[1], "typeID"), Some(&PyRep::Integer(i64::from(container_type_id))));
    let inv_notif = notifs
        .iter()
        .find(|n| n.event == "OnInvalidateCacheForLocations")
        .expect("OnInvalidateCacheForLocations");
    assert_eq!(inv_notif.args, vec![PyRep::List(vec![PyRep::Integer(container_id)])]);

    // Omitting oldPassword fails with InvalidPassword when existing password is set
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess_a,
            "invbroker",
            Some(oid),
            "SetPassword",
            &[PyRep::Integer(2), PyRep::String("beta456".into())],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap_err()
        .code,
        "InvalidPassword"
    );
    assert!(ctx.drain_notifications_for_char(PILOT_A).iter().all(|n| n.event != "OnPasswordChanged"));

    // Setting and clearing general password (which = 1) with None does not collide with which = 2
    ctx.dispatch_rpc(
        &mut sess_a,
        "invbroker",
        Some(oid),
        "SetPassword",
        &[PyRep::Integer(1), PyRep::String("gen111".into()), PyRep::String(String::new())],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SetPassword which=1");
    ctx.dispatch_rpc(
        &mut sess_a,
        "invbroker",
        Some(oid),
        "SetPassword",
        &[PyRep::Integer(1), PyRep::None, PyRep::String("gen111".into())],
        &HashMap::new(),
        None,
    )
    .await
    .expect("Clear SetPassword which=1 with None");
    // Config password (which = 2) still requires "alpha123"
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess_a,
            "invbroker",
            Some(oid),
            "SetPassword",
            &[PyRep::Integer(2), PyRep::String("beta456".into()), PyRep::String(String::new())],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap_err()
        .code,
        "InvalidPassword"
    );

    // Corp container password requires ROLE_DIRECTOR | ROLE_EQUIPMENT_CONFIG
    sess_a.corporation_id = Some(2001);
    sess_a.corp_role = 0;
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "invbroker",
            "SetPassword",
            &[PyRep::Integer(corp_container_id), PyRep::Integer(1), PyRep::String("corp_pw".into()), PyRep::String(String::new())],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleRequired"
    );
    sess_a.corp_role = ROLE_EQUIPMENT_CONFIG | ROLE_DIRECTOR;
    rpc(
        &ctx,
        &mut sess_a,
        "invbroker",
        "SetPassword",
        &[PyRep::Integer(corp_container_id), PyRep::Integer(1), PyRep::String("corp_pw".into()), PyRep::String(String::new())],
    )
    .await
    .expect("corp container SetPassword with equipment config role");

    // BF-04 negative checks: invalid which, non-container item, non-string password, and >2500m in space
    assert_eq!(rpc(&ctx, &mut sess_a, "invbroker", "SetPassword", &[PyRep::Integer(corp_container_id), PyRep::Integer(99), PyRep::String("pw".into())]).await.unwrap_err().code, "InvalidPasswordType");
    assert_eq!(rpc(&ctx, &mut sess_a, "invbroker", "SetPassword", &[PyRep::Integer(corp_container_id), PyRep::Integer(1), PyRep::Integer(123)]).await.unwrap_err().code, "InvalidPassword");
    let (mineral_id, ship_sp_id, far_cont_id) = (960_000_031_i64, 960_000_032_i64, 960_000_033_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mineral_id, make_item(mineral_id, 34, PILOT_A, STATION_JITA, InventoryFlag::Hangar, 100, false));
        snap.items.insert(far_cont_id, make_item(far_cont_id, 3465, PILOT_A, SYS_JITA as i64, InventoryFlag::None, 1, true));
    });
    assert_eq!(rpc(&ctx, &mut sess_a, "invbroker", "SetPassword", &[PyRep::Integer(mineral_id), PyRep::Integer(1), PyRep::String("pw".into())]).await.unwrap_err().code, "ContainerDoesNotSupportPassword");
    let mut sess_space = login_in_space(&ctx, 9_609, "demo", "demo-secret", PILOT_A, ship_sp_id, 587, SYS_JITA, DVec3::ZERO).await;
    let sys_sp = ctx.systems.get_or_spawn(SYS_JITA);
    sys_sp.spawn_ball_with_meta(eve_destiny::Ball { id: far_cont_id, type_id: 3465, position: DVec3::new(10_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000.0, radius: 50.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop }, ctx.build_ship_combat_state(PILOT_A, far_cont_id, 3465), ctx.build_ship_slim_meta(PILOT_A, 3465)).await.unwrap();
    assert_eq!(rpc(&ctx, &mut sess_space, "invbroker", "SetPassword", &[PyRep::Integer(far_cont_id), PyRep::Integer(1), PyRep::String("pw".into())]).await.unwrap_err().code, "OutsideOperationalRange");
}

}
