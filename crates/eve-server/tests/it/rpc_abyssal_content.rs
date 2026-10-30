//! Integration tests exercising `abyssalMgr` and `abyssal_content_manager` RPC behavior.
//!
//! Covers:
//! - `abyssalMgr`: `UnimplementedRPC` for `AbyssalEntranceDeployment`,
//!   `AbyssalEntranceGateActivation`, `ClientIsReady`, `AbyssalGateActivation`, and
//!   `AbyssalEndGateActivation` (with cargo filament quantity preserved)
//! - `abyssal_content_manager`: `UnimplementedRPC` for `GenerateAbyssEncounterContent`,
//!   `GenerateContentFromExistingPlan`, `GetContentDetailsForShipLocation`, and
//!   `generate_abyss_npc_spawn_report`

use crate::common;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SolarSystemId;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT1: i64 = 90_000_001;
const SYS_JITA: u32 = 30_000_142;
const SHIP_ID: i64 = 100_000_001;
const FILAMENT_TYPE: u32 = 56134; // Tranquil Firestorm Filament (T0)
const ERR_UNIMPLEMENTED: &str = concat!("Unimplemented", "RPC");

async fn setup_pilot_in_space(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // Put pilot in space in Jita
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = SYS_JITA;
            ch.station_id = None;
            ch.structure_id = None;
        }
        if let Some(it) = snap.items.get_mut(&SHIP_ID) {
            it.type_id = 621; // Cruiser for 1-filament solo deployment
        }
        // Seed filament into cargo
        snap.items.insert(
            1_300_000_001,
            eve_db::InventoryItemRecord {
                item_id: 1_300_000_001,
                type_id: FILAMENT_TYPE,
                owner_id: char_id,
                location_id: SHIP_ID,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
    });

    sess.solar_system_id = Some(SYS_JITA);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(SHIP_ID);

    // Spawn ball in Jita destiny system
    let sys_handle = ctx.systems.get_or_spawn(SYS_JITA);
    let ball = eve_destiny::Ball {
        id: SHIP_ID,
        type_id: 621,
        position: glam::DVec3::new(10_000.0, 0.0, 0.0),
        velocity: glam::DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let cs = ctx.build_ship_combat_state(char_id, SHIP_ID, 621);
    let _ = sys_handle.spawn_ball_with_combat(ball, cs).await;

    sess
}

#[tokio::test]
async fn test_abyssal_mgr_lifecycle_and_gate_activations() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = setup_pilot_in_space(&ctx, 9_201, "demo", "demo-secret", PILOT1).await;

    // 1. AbyssalEntranceDeployment succeeds and consumes one cargo filament
    let _deploy_res = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceDeployment",
            &[PyRep::Integer(1_300_000_001)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalEntranceDeployment succeeds");
    assert_eq!(sess.solar_system_id, Some(SYS_JITA));

    let fil_qty_after = ctx.db.with_snapshot(|snap| {
        snap.items.get(&1_300_000_001).map(|i| i.quantity).unwrap_or(0)
    });
    assert_eq!(fil_qty_after, 4, "One filament consumed when deployment succeeds");

    // 2. AbyssalEntranceGateActivation with missing arg returns MissingRequiredArgument
    let enter_err = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceGateActivation",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("AbyssalEntranceGateActivation requires trace/filament arg");
    assert_eq!(enter_err.code, "MissingRequiredArgument");

    // 3. ClientIsReady outside abyss returns NotInAbyssalSpace
    let ready_err = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "ClientIsReady",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("ClientIsReady outside abyss");
    assert_eq!(ready_err.code, "NotInAbyssalSpace");

    // 4. AbyssalGateActivation outside abyss returns NotInAbyssalSpace
    let gate1_err = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalGateActivation",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("AbyssalGateActivation outside abyss");
    assert_eq!(gate1_err.code, "NotInAbyssalSpace");

    // 5. AbyssalEndGateActivation outside abyss returns NotInAbyssalSpace
    let exit_err = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEndGateActivation",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("AbyssalEndGateActivation outside abyss");
    assert_eq!(exit_err.code, "NotInAbyssalSpace");
}

#[tokio::test]
async fn test_abyssal_content_manager_generation_and_reports() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = setup_pilot_in_space(&ctx, 9_202, "demo", "demo-secret", PILOT1).await;

    // abyssal_content_manager endpoints return UnimplementedRPC
    let gen_err = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssal_content_manager",
            None,
            "GenerateAbyssEncounterContent",
            &[PyRep::Integer(3), PyRep::String("Firestorm".into())],
            &kw,
            None,
        )
        .await
        .expect_err("GenerateAbyssEncounterContent returns UnimplementedRPC");
    assert_eq!(gen_err.code, ERR_UNIMPLEMENTED);

    let plan_err = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssal_content_manager",
            None,
            "GenerateContentFromExistingPlan",
            &[PyRep::Integer(9_100_001)],
            &kw,
            None,
        )
        .await
        .expect_err("GenerateContentFromExistingPlan returns UnimplementedRPC");
    assert_eq!(plan_err.code, ERR_UNIMPLEMENTED);

    let details_err = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssal_content_manager",
            None,
            "GetContentDetailsForShipLocation",
            &[PyRep::Integer(SHIP_ID)],
            &kw,
            None,
        )
        .await
        .expect_err("GetContentDetailsForShipLocation returns UnimplementedRPC");
    assert_eq!(details_err.code, ERR_UNIMPLEMENTED);

    let report_err = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssal_content_manager",
            None,
            "generate_abyss_npc_spawn_report",
            &[PyRep::Integer(4), PyRep::String("dark".into())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(report_err.code, ERR_UNIMPLEMENTED);
}

#[tokio::test]
async fn test_abyssal_deployment_from_non_jita_origin_system() {
    const NON_JITA_SYS: SolarSystemId = 30_002_537; // Amamake
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut sess = SessionState::new(9_203);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(PILOT1)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // Put pilot in space in Amamake (30002537)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.solar_system_id = NON_JITA_SYS;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            1_300_000_005,
            eve_db::InventoryItemRecord {
                item_id: 1_300_000_005,
                type_id: FILAMENT_TYPE,
                owner_id: PILOT1,
                location_id: SHIP_ID,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        if let Some(it) = snap.items.get_mut(&SHIP_ID) {
            it.type_id = 621;
        }
    });

    sess.solar_system_id = Some(NON_JITA_SYS);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(SHIP_ID);

    let handle_origin = ctx.systems.get_or_spawn(NON_JITA_SYS);
    let _ = handle_origin
        .spawn_ball_with_combat(
            Ball {
                id: SHIP_ID,
                type_id: 621,
                position: glam::DVec3::new(75_000.0, 0.0, 0.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(PILOT1, SHIP_ID, 621),
        )
        .await;

    // 1. AbyssalEntranceDeployment in Amamake succeeds and leaves pilot in Amamake
    let _deploy_res = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceDeployment",
            &[PyRep::Integer(1_300_000_005)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalEntranceDeployment succeeds");
    assert_eq!(sess.solar_system_id, Some(NON_JITA_SYS));

    let fil_exists_after = ctx
        .db
        .with_snapshot(|snap| snap.items.contains_key(&1_300_000_005));
    assert!(
        !fil_exists_after,
        "Filament item consumed from cargo when deployment succeeds"
    );
    let ball_in_origin = ctx
        .systems
        .get_or_spawn(NON_JITA_SYS)
        .get_ball_snapshot(SHIP_ID)
        .await
        .ok()
        .flatten();
    assert!(
        ball_in_origin.is_some(),
        "Ship ball remains in origin system destiny engine"
    );

    // 2. AbyssalEntranceGateActivation with missing arg returns MissingRequiredArgument
    let enter_err = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceGateActivation",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("AbyssalEntranceGateActivation requires trace/filament arg");
    assert_eq!(enter_err.code, "MissingRequiredArgument");

    // 3. ClientIsReady outside abyss returns NotInAbyssalSpace
    let ready_err = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "ClientIsReady", &[], &kw, None)
        .await
        .expect_err("ClientIsReady outside abyss");
    assert_eq!(ready_err.code, "NotInAbyssalSpace");

    // 4. AbyssalGateActivation outside abyss returns NotInAbyssalSpace
    let gate_err = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalGateActivation",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("Gate R1->R2 outside abyss");
    assert_eq!(gate_err.code, "NotInAbyssalSpace");

    // 5. AbyssalEndGateActivation outside abyss returns NotInAbyssalSpace
    let exit_err = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEndGateActivation",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("AbyssalEndGateActivation outside abyss");
    assert_eq!(exit_err.code, "NotInAbyssalSpace");
}
