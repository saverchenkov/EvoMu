//! Integration tests exercising `abyssalMgr` and `abyssal_content_manager` RPC behavior.
//!
//! Covers:
//! - `abyssalMgr`: `AbyssalEntranceDeployment` (cargo consumption and destiny ball placement in
//!   SDE Abyssal Deadspace system), and `UnimplementedRPC` for `AbyssalEntranceGateActivation`,
//!   `ClientIsReady`, `AbyssalGateActivation`, and `AbyssalEndGateActivation`
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
const FILAMENT_TYPE: u32 = 47763; // Calm Firestorm Filament
const ERR_UNIMPLEMENTED: &str = concat!("Unimplemented", "RPC");

fn sde_abyssal_system_id(ctx: &ServerContext) -> SolarSystemId {
    ctx.sde
        .solar_systems
        .values()
        .filter(|s| (12_000_000..13_000_000).contains(&s.region_id))
        .map(|s| s.solar_system_id)
        .min()
        .expect("SDE contains Abyssal Deadspace solar systems")
}

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
        type_id: 601,
        position: glam::DVec3::new(10_000.0, 0.0, 0.0),
        velocity: glam::DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let cs = ctx.build_ship_combat_state(char_id, SHIP_ID, 601);
    let _ = sys_handle.spawn_ball_with_combat(ball, cs).await;

    sess
}

#[tokio::test]
async fn test_abyssal_mgr_lifecycle_and_gate_activations() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = setup_pilot_in_space(&ctx, 9_201, "demo", "demo-secret", PILOT1).await;
    let expected_abyss_sys = sde_abyssal_system_id(&ctx);

    // 1. AbyssalEntranceDeployment: consumes requested filament item and enters SDE Abyssal system
    let deploy_rep = ctx
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
        .expect("AbyssalEntranceDeployment");
    assert_eq!(
        deploy_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(expected_abyss_sys as i64)
    );
    assert_eq!(sess.solar_system_id, Some(expected_abyss_sys));

    // Verify filament was consumed on deployment and ship ball is placed in destination destiny engine
    let fil_qty_after = ctx.db.with_snapshot(|snap| {
        snap.items.get(&1_300_000_001).map(|i| i.quantity).unwrap_or(0)
    });
    assert_eq!(fil_qty_after, 4, "Filament consumed on deployment");
    let ball_in_sys = ctx
        .systems
        .get_or_spawn(expected_abyss_sys)
        .get_ball_snapshot(SHIP_ID)
        .await
        .ok()
        .flatten();
    assert!(
        ball_in_sys.is_some(),
        "Ship ball is present in destination Abyssal system destiny engine"
    );

    // 2. AbyssalEntranceGateActivation returns UnimplementedRPC
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
        .expect_err("AbyssalEntranceGateActivation returns UnimplementedRPC");
    assert_eq!(enter_err.code, "UnimplementedRPC");

    // 3. ClientIsReady returns UnimplementedRPC
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
        .expect_err("ClientIsReady returns UnimplementedRPC");
    assert_eq!(ready_err.code, "UnimplementedRPC");

    // 4. AbyssalGateActivation returns UnimplementedRPC
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
        .expect_err("AbyssalGateActivation returns UnimplementedRPC");
    assert_eq!(gate1_err.code, "UnimplementedRPC");

    // 5. AbyssalEndGateActivation returns UnimplementedRPC
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
        .expect_err("AbyssalEndGateActivation returns UnimplementedRPC");
    assert_eq!(exit_err.code, "UnimplementedRPC");
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
    assert_eq!(gen_err.code, "UnimplementedRPC");

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
    assert_eq!(plan_err.code, "UnimplementedRPC");

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
    assert_eq!(details_err.code, "UnimplementedRPC");

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
    let expected_abyss_sys = sde_abyssal_system_id(&ctx);

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
                type_id: 601,
                position: glam::DVec3::new(75_000.0, 0.0, 0.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(PILOT1, SHIP_ID, 601),
        )
        .await;

    // 1. Deploy filament in Amamake
    let deploy_rep = ctx
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
        .expect("AbyssalEntranceDeployment in Amamake");
    assert_eq!(
        deploy_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(expected_abyss_sys as i64)
    );
    assert_eq!(sess.solar_system_id, Some(expected_abyss_sys));

    // Verify filament stack of 1 was removed from cargo and ship ball is placed in destination destiny engine
    let fil_exists_after = ctx
        .db
        .with_snapshot(|snap| snap.items.contains_key(&1_300_000_005));
    assert!(
        !fil_exists_after,
        "Single filament item removed from cargo on deployment"
    );
    let ball_in_sys = ctx
        .systems
        .get_or_spawn(expected_abyss_sys)
        .get_ball_snapshot(SHIP_ID)
        .await
        .ok()
        .flatten();
    assert!(
        ball_in_sys.is_some(),
        "Ship ball is present in destination Abyssal system destiny engine"
    );

    // 2. AbyssalEntranceGateActivation returns UnimplementedRPC
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
        .expect_err("AbyssalEntranceGateActivation returns UnimplementedRPC");
    assert_eq!(enter_err.code, "UnimplementedRPC");

    // 3. ClientIsReady returns UnimplementedRPC
    let ready_err = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "ClientIsReady", &[], &kw, None)
        .await
        .expect_err("ClientIsReady returns UnimplementedRPC");
    assert_eq!(ready_err.code, "UnimplementedRPC");

    // 4. AbyssalGateActivation returns UnimplementedRPC
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
        .expect_err("Gate R1->R2 returns UnimplementedRPC");
    assert_eq!(gate_err.code, "UnimplementedRPC");

    // 5. AbyssalEndGateActivation returns UnimplementedRPC
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
        .expect_err("AbyssalEndGateActivation returns UnimplementedRPC");
    assert_eq!(exit_err.code, "UnimplementedRPC");
}
