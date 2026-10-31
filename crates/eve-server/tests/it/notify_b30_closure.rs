//! Live integration tests for Batch 30 notification emitters (`notify_emitters_b30.rs`).
//!
//! Covers 12 `notify:` parity rows:
//! - `notify:OnOperationReplayed`
//! - `notify:OnUiRevealForced`
//! - `notify:OnPasswordChanged`
//! - `notify:OnInvalidateCacheForLocations`
//! - `notify:OnMultiEvent`
//! - `notify:OnSolarSystemLPChange`
//! - `notify:OnVictoryPointStateUpdated`
//! - `notify:OnWeaponsTimerUpdate`
//! - `notify:OnPvpTimerUpdate`
//! - `notify:OnCriminalTimerUpdate`
//! - `notify:OnSystemCriminalFlagUpdates`
//! - `notify:OnShipDeath`

#![forbid(unsafe_code)]

use crate::common;
use crate::notify_real_emitters_batch7::{login_in_space, make_item};
use eve_db::InventoryFlag;
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
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
// parity: notify:OnPasswordChanged, notify:OnInvalidateCacheForLocations
#[tokio::test]
async fn notify_b30_operations_and_container_password() {
    eve_testkit::attest(&[
        "notify:OnOperationReplayed",
        "notify:OnUiRevealForced",
        "notify:OnPasswordChanged",
        "notify:OnInvalidateCacheForLocations",
    ]);

    let ctx = new_ctx();
    let mut unauth = SessionState::new(9_600);
    assert_eq!(
        rpc(
            &ctx,
            &mut unauth,
            "slash",
            "SlashCmd",
            &[PyRep::String("/operation reveal_ui".into())],
        )
        .await
        .unwrap_err()
        .code,
        "Unauthorized"
    );

    let mut sess_a = SessionState::new(9_601);
    common::login_pilot(&ctx, &mut sess_a, "demo", "demo-secret").await;
    rpc(
        &ctx,
        &mut sess_a,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(PILOT_A)],
    )
    .await
    .expect("SelectCharacterID A");
    sess_a.role |= ROLE_GM_DEFAULT;
    sess_a.station_id = Some(STATION_JITA);
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    // 1. OnOperationReplayed: negative invalid op rejected without emitting
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "slash",
        "SlashCmd",
        &[PyRep::String("/operation replay 0 0".into())],
    )
    .await
    .is_err());
    assert!(ctx
        .drain_notifications_for_char(PILOT_A)
        .iter()
        .all(|n| n.event != "OnOperationReplayed"));

    // Positive: /operation replay 1 1 mutates CharacterOperationsRecord and emits OnOperationReplayed(1, 1)
    let replay_res = rpc(
        &ctx,
        &mut sess_a,
        "slash",
        "SlashCmd",
        &[PyRep::String("/operation replay 1 1".into())],
    )
    .await
    .expect("SlashCmd /operation replay 1 1");
    assert_eq!(replay_res, PyRep::Bool(true));
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    let replayed = notifs
        .iter()
        .find(|n| n.event == "OnOperationReplayed")
        .expect("OnOperationReplayed emitted");
    assert_eq!(replayed.id_type, "charid");
    assert_eq!(replayed.args, vec![PyRep::Integer(1), PyRep::Integer(1)]);

    // 2. OnUiRevealForced: /operation reveal_ui emits OnUiRevealForced() with 0 args
    let reveal_res = rpc(
        &ctx,
        &mut sess_a,
        "slash",
        "SlashCmd",
        &[PyRep::String("/operation reveal_ui".into())],
    )
    .await
    .expect("SlashCmd /operation reveal_ui");
    assert_eq!(reveal_res, PyRep::Bool(true));
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    let revealed = notifs
        .iter()
        .find(|n| n.event == "OnUiRevealForced")
        .expect("OnUiRevealForced emitted");
    assert_eq!(revealed.id_type, "charid");
    assert!(revealed.args.is_empty());

    // 3 & 4. OnPasswordChanged & OnInvalidateCacheForLocations via bound invbroker.SetPassword
    let container_id: i64 = 99_300_001;
    let container_type_id: u32 = 3465; // Small Secure Container
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            container_id,
            make_item(
                container_id,
                container_type_id,
                PILOT_A,
                STATION_JITA,
                InventoryFlag::Hangar,
                1,
                true,
            ),
        );
    });
    let bind = rpc(
        &ctx,
        &mut sess_a,
        "invbroker",
        "MachoBindObject",
        &[
            PyRep::Tuple(vec![PyRep::Integer(STATION_JITA), PyRep::Integer(15)]),
            PyRep::None,
        ],
    )
    .await
    .expect("MachoBindObject");
    let PyRep::Tuple(bind_parts) = &bind else {
        panic!("expected MachoBindObject tuple");
    };
    let mgr_oid = bind_parts[0].as_i64().expect("mgr bound id") as u64;
    let bound_rep = ctx
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
        .expect("GetInventoryFromId");
    let oid = bound_rep.as_i64().expect("container bound id") as u64;
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    // Set initial config password (which = 2)
    let set_res = ctx
        .dispatch_rpc(
            &mut sess_a,
            "invbroker",
            Some(oid),
            "SetPassword",
            &[
                PyRep::Integer(2),
                PyRep::String("alpha123".into()),
                PyRep::String(String::new()),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("bound SetPassword");
    assert!(set_res != PyRep::None);

    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    let pw_notif = notifs
        .iter()
        .find(|n| n.event == "OnPasswordChanged")
        .expect("OnPasswordChanged emitted");
    assert_eq!(pw_notif.args.len(), 2);
    assert_eq!(pw_notif.args[0], PyRep::Integer(2));
    assert_eq!(
        kv_get(&pw_notif.args[1], "itemID"),
        Some(&PyRep::Integer(container_id))
    );
    assert_eq!(
        kv_get(&pw_notif.args[1], "typeID"),
        Some(&PyRep::Integer(i64::from(container_type_id)))
    );

    let inv_notif = notifs
        .iter()
        .find(|n| n.event == "OnInvalidateCacheForLocations")
        .expect("OnInvalidateCacheForLocations emitted");
    assert_eq!(
        inv_notif.args,
        vec![PyRep::List(vec![PyRep::Integer(container_id)])]
    );

    // Negative: wrong oldPassword rejected with InvalidPassword without emitting
    let bad_old = ctx
        .dispatch_rpc(
            &mut sess_a,
            "invbroker",
            Some(oid),
            "SetPassword",
            &[
                PyRep::Integer(2),
                PyRep::String("beta456".into()),
                PyRep::String("wrong_old".into()),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(bad_old.code, "InvalidPassword");
    assert!(ctx
        .drain_notifications_for_char(PILOT_A)
        .iter()
        .all(|n| n.event != "OnPasswordChanged"));
}

// parity: notify:OnMultiEvent, notify:OnSolarSystemLPChange, notify:OnVictoryPointStateUpdated
#[tokio::test]
async fn notify_b30_target_multi_event_and_fw_warzone() {
    eve_testkit::attest(&[
        "notify:OnMultiEvent",
        "notify:OnSolarSystemLPChange",
        "notify:OnVictoryPointStateUpdated",
    ]);

    let ctx = new_ctx();
    let (ship_a, ship_b) = (960_000_101_i64, 960_000_102_i64);
    let mut sess_a = login_in_space(&ctx, 9_611, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;
    let _sess_b = login_in_space(&ctx, 9_612, "other", "other-secret", PILOT_B, ship_b, 587, SYS_JITA, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let sys = ctx.systems.get_or_spawn(SYS_JITA);
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    // 5. OnMultiEvent: AddTarget(ship_b) + step_manual -> PILOT_B receives otheradd; RemoveTarget -> otherlost
    assert!(rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(0)])
        .await
        .is_err());
    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(ship_b)],
    )
    .await
    .expect("AddTarget");
    sys.step_manual(10.0).await.expect("step_manual");

    let notifs_b = ctx.drain_notifications_for_char(PILOT_B);
    let multi_add = notifs_b
        .iter()
        .find(|n| n.event == "OnMultiEvent")
        .expect("PILOT_B OnMultiEvent otheradd");
    assert_eq!(
        multi_add.args,
        vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Tuple(vec![
                PyRep::String("OnTarget".into()),
                PyRep::String("otheradd".into()),
                PyRep::Integer(ship_a),
                PyRep::String(String::new()),
            ]),
            PyRep::Integer(0),
        ])])]
    );

    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "RemoveTarget",
        &[PyRep::Integer(ship_b)],
    )
    .await
    .expect("RemoveTarget");
    let notifs_b_lost = ctx.drain_notifications_for_char(PILOT_B);
    let multi_lost = notifs_b_lost
        .iter()
        .find(|n| n.event == "OnMultiEvent")
        .expect("PILOT_B OnMultiEvent otherlost");
    assert_eq!(
        multi_lost.args,
        vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Tuple(vec![
                PyRep::String("OnTarget".into()),
                PyRep::String("otherlost".into()),
                PyRep::Integer(ship_a),
                PyRep::String(String::new()),
            ]),
            PyRep::Integer(0),
        ])])]
    );

    // 6. OnSolarSystemLPChange: facWarMgr.DonateLPsToSolarSystem
    let fw_sys: u32 = 30_002_537; // Amamake (warzone low-sec)
    let caldari_militia_corp: i64 = 1_000_180;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = fw_sys;
            ch.war_faction_id = Some(500_001);
            ch.loyalty_points.insert(caldari_militia_corp, 5_000);
        }
        if let Some(occ) = snap.fw_system_occupations.get_mut(&fw_sys) {
            occ.owner_faction_id = 500_001;
            occ.occupier_faction_id = 500_001;
            occ.victory_points = 1_000;
            occ.victory_points_threshold = 3_000;
        }
    });
    sess_a.war_faction_id = Some(500_001);
    sess_a.solar_system_id = Some(fw_sys);
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    // Negative: donating more LP than available fails with InsufficientLoyaltyPoints
    let err_lp = rpc(
        &ctx,
        &mut sess_a,
        "facWarMgr",
        "DonateLPsToSolarSystem",
        &[PyRep::Integer(i64::from(fw_sys)), PyRep::Integer(99_999)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_lp.code, "InsufficientLoyaltyPoints");
    assert!(ctx
        .drain_notifications_for_char(PILOT_A)
        .iter()
        .all(|n| n.event != "OnSolarSystemLPChange"));

    // Positive: donating 1,500 LP deducts LP and emits OnSolarSystemLPChange(0, 1500)
    rpc(
        &ctx,
        &mut sess_a,
        "facWarMgr",
        "DonateLPsToSolarSystem",
        &[PyRep::Integer(i64::from(fw_sys)), PyRep::Integer(1_500)],
    )
    .await
    .expect("DonateLPsToSolarSystem");
    let lp_notifs = ctx.drain_notifications_for_char(PILOT_A);
    let lp_ev = lp_notifs
        .iter()
        .find(|n| n.event == "OnSolarSystemLPChange")
        .expect("OnSolarSystemLPChange emitted");
    assert_eq!(lp_ev.id_type, "solarsystemid");
    assert_eq!(lp_ev.args, vec![PyRep::Integer(0), PyRep::Integer(1_500)]);

    // 7. OnVictoryPointStateUpdated: warzone VP update emits OnVictoryPointStateUpdated(fw_sys, vp_state)
    let (new_vp, contested) = eve_server::services::fw_services::add_victory_points(&ctx, fw_sys, 300);
    assert_eq!(new_vp, 1_300);
    assert!(contested > 40.0);
    let vp_notifs = ctx.drain_notifications_for_char(PILOT_A);
    let vp_ev = vp_notifs
        .iter()
        .find(|n| n.event == "OnVictoryPointStateUpdated")
        .expect("OnVictoryPointStateUpdated emitted");
    assert_eq!(vp_ev.id_type, "solarsystemid");
    assert_eq!(vp_ev.args.len(), 2);
    assert_eq!(vp_ev.args[0], PyRep::Integer(i64::from(fw_sys)));
    assert_eq!(
        kv_get(&vp_ev.args[1], "victoryPoints"),
        Some(&PyRep::Integer(1_300))
    );
}

// parity: notify:OnWeaponsTimerUpdate, notify:OnPvpTimerUpdate, notify:OnCriminalTimerUpdate
// parity: notify:OnSystemCriminalFlagUpdates, notify:OnShipDeath
#[tokio::test]
async fn notify_b30_crimewatch_timers_and_ship_death() {
    eve_testkit::attest(&[
        "notify:OnWeaponsTimerUpdate",
        "notify:OnPvpTimerUpdate",
        "notify:OnCriminalTimerUpdate",
        "notify:OnSystemCriminalFlagUpdates",
        "notify:OnShipDeath",
    ]);

    let ctx = new_ctx();
    let (ship_a, ship_b) = (960_000_201_i64, 960_000_202_i64);
    let _sess_a = login_in_space(&ctx, 9_621, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;
    let _sess_b = login_in_space(&ctx, 9_622, "other", "other-secret", PILOT_B, ship_b, 587, SYS_JITA, DVec3::new(500.0, 0.0, 0.0)).await;
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    // 8-11. Real high-sec aggression triggers OnWeaponsTimerUpdate, OnPvpTimerUpdate,
    // OnCriminalTimerUpdate, and OnSystemCriminalFlagUpdates
    let _ = eve_server::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        &ctx,
        PILOT_A,
        PILOT_B,
        SYS_JITA,
        ship_b,
        true,
    );

    let notifs_a = ctx.drain_notifications_for_char(PILOT_A);
    let weap = notifs_a
        .iter()
        .find(|n| n.event == "OnWeaponsTimerUpdate")
        .expect("OnWeaponsTimerUpdate");
    assert_eq!(weap.args[0], PyRep::Integer(101));

    let pvp = notifs_a
        .iter()
        .find(|n| n.event == "OnPvpTimerUpdate")
        .expect("OnPvpTimerUpdate");
    assert_eq!(pvp.args[0], PyRep::Integer(201));

    let crim = notifs_a
        .iter()
        .find(|n| n.event == "OnCriminalTimerUpdate")
        .expect("OnCriminalTimerUpdate");
    assert_eq!(crim.args[0], PyRep::Integer(301));

    let sys_crim = notifs_a
        .iter()
        .find(|n| n.event == "OnSystemCriminalFlagUpdates")
        .expect("OnSystemCriminalFlagUpdates");
    assert_eq!(sys_crim.id_type, "solarsystemid");
    assert_eq!(sys_crim.args.len(), 3);
    assert_eq!(sys_crim.args[2], PyRep::List(vec![PyRep::Integer(PILOT_A)]));

    // 12. OnShipDeath: destroying PILOT_B's ship emits OnShipDeath() with 0 args and ejects pod
    let _ = ctx.drain_notifications_for_char(PILOT_B);
    let death_res = eve_server::combat::death_pipeline::execute_death_pipeline(
        &ctx,
        SYS_JITA,
        PILOT_B,
        ship_b,
        PILOT_A,
        DVec3::new(500.0, 0.0, 0.0),
        42,
    )
    .await
    .expect("execute_death_pipeline");
    assert!(death_res.ejected_capsule_id.unwrap_or(0) > 0);
    let notifs_b = ctx.drain_notifications_for_char(PILOT_B);
    let death = notifs_b
        .iter()
        .find(|n| n.event == "OnShipDeath")
        .expect("OnShipDeath emitted");
    assert_eq!(death.id_type, "charid");
    assert!(death.args.is_empty());
}
