//! Live client-RPC notification closure tests (`notify:On*`).
//!
//! Verifies server-to-client V24 notifications emitted by real domain RPCs
//! (`abyssalMgr`, `voidSpaceMgr`, `seasonManager`, `raffleProxy`, `dogmaIM`, `slash`, `allianceRegistry`).

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::season_manager::{SeasonalChallengeDef, SEASONAL_CHALLENGES_CONFIG_KEY};
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;
const FILAMENT_TYPE_ID: u32 = 56_134; // Tranquil Firestorm Filament (T0, group 1979)

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
        ..Default::default()
    }
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

async fn login_in_space(
    ctx: &ServerContext,
    conn_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let mut sess = SessionState::new(conn_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        let mut ship = make_item(ship_id, ship_type_id, char_id, sys_id as i64, InventoryFlag::None, 1, true);
        ship.dynamic_attributes.insert(48, 25_000.0);
        ship.dynamic_attributes.insert(11, 25_000.0);
        snap.items.insert(ship_id, ship);
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    cs.capacitor = 50_000.0;
    cs.max_capacitor = 50_000.0;
    let ball = Ball {
        id: ship_id,
        type_id: ship_type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 10_000_000.0,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle.spawn_ball_with_combat(ball, cs).await;
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], name: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(e, _)| e == name)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| panic!("Expected notification {name}, got {:?}", notifs.iter().map(|(e, _)| e).collect::<Vec<_>>()))
}

// parity: notify:OnAbyssalLocationEntered, notify:OnAbyssalContentFinished
// parity: notify:OnVoidSpaceEntered, notify:OnVoidSpaceRelog, notify:OnVoidSpaceContentFinished
// parity: notify:OnSeasonalGoalCompleted, notify:OnSeasonOmegaRewardsClaimed
#[tokio::test]
async fn test_notify_live_abyssal_void_and_seasons() {
    eve_testkit::attest(&[
        "notify:OnAbyssalLocationEntered",
        "notify:OnAbyssalContentFinished",
        "notify:OnVoidSpaceEntered",
        "notify:OnVoidSpaceRelog",
        "notify:OnVoidSpaceContentFinished",
        "notify:OnSeasonalGoalCompleted",
        "notify:OnSeasonOmegaRewardsClaimed",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let (ship_id, ship_b_id, fil_id) = (640_000_001_i64, 640_000_002_i64, 640_000_010_i64);
    let mut sess = login_in_space(&ctx, 9_401, "demo", "demo-secret", PILOT_A, ship_id, 621, SYS_JITA, DVec3::ZERO).await;
    let mut sess_b = login_in_space(&ctx, 9_402, "other", "other-secret", PILOT_B, ship_b_id, 621, SYS_JITA, DVec3::new(1_000.0, 0.0, 0.0)).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.clone_grade = 2;
        }
        if let Some(ch) = snap.characters.get_mut(&PILOT_B) {
            ch.clone_grade = 1;
            ch.station_id = Some(JITA_STATION);
        }
        snap.items.insert(fil_id, make_item(fil_id, FILAMENT_TYPE_ID, PILOT_A, ship_id, InventoryFlag::Cargo, 3, false));
        let mut defs = eve_server::services::season_manager::default_seasonal_challenge_definitions();
        defs.insert(0, SeasonalChallengeDef {
            challenge_id: 1_005,
            season_id: 1,
            event_type: "ui_onboarding_step".into(),
            title: "Onboarding".into(),
            description: "Complete onboarding step".into(),
            points: 20,
            max_progress: 1,
            client_event_driven: true,
            reward_type_id: 0,
            reward_quantity: 0,
            is_dormant: false,
            ..Default::default()
        });
        snap.service_state_blobs.insert(SEASONAL_CHALLENGES_CONFIG_KEY.into(), serde_json::to_string(&defs).expect("defs"));
    });
    sess_b.station_id = Some(JITA_STATION);

    // 1. Abyssal entrance, 3 rooms, and exit -> OnAbyssalLocationEntered + OnAbyssalContentFinished
    let deploy_rep = rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(fil_id)]).await.expect("Deploy");
    let trace_id = deploy_rep.dict_get("traceBallID").and_then(|v| v.as_i64()).expect("traceBallID");
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    let enter_rep = rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_id)]).await.expect("Enter Abyss");
    let pocket_id = enter_rep.dict_get("pocketID").and_then(|v| v.as_i64()).expect("pocketID");
    let abyss_in_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&abyss_in_notifs, "OnAbyssalLocationEntered")[0].dict_get("content_id").and_then(|v| v.as_i64()), Some(pocket_id));

    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_A, 0).await;
    rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalGateActivation", &[PyRep::Integer(1_895_000_000 + pocket_id * 10 + 1)]).await.expect("R1->R2");
    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_A, 1).await;
    rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalGateActivation", &[PyRep::Integer(1_895_000_000 + pocket_id * 10 + 2)]).await.expect("R2->R3");
    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_A, 2).await;
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEndGateActivation", &[PyRep::Integer(1_895_000_000 + pocket_id * 10 + 3)]).await.expect("Exit Abyss");
    let abyss_out_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&abyss_out_notifs, "OnAbyssalContentFinished"), &[PyRep::Integer(pocket_id), PyRep::Integer(1)]);

    // 2. Void Space jump, relog ClientIsReady, and exit -> OnVoidSpaceEntered, OnVoidSpaceRelog, OnVoidSpaceContentFinished
    ctx.db.with_mut_snapshot(|s| s.items.get_mut(&fil_id).unwrap().type_id = 62_448);
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpacePlayerJump", &[PyRep::Integer(fil_id), PyRep::Integer(777_001)]).await.expect("VoidSpacePlayerJump");
    let void_in_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&void_in_notifs, "OnVoidSpaceEntered")[0].dict_get("content_id").and_then(|v| v.as_i64()), Some(fil_id));

    rpc(&ctx, &mut sess, "voidSpaceMgr", "ClientIsReady", &[]).await.expect("voidSpaceMgr.ClientIsReady");
    let relog_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&relog_notifs, "OnVoidSpaceRelog")[0].dict_get("content_id").and_then(|v| v.as_i64()), Some(fil_id));

    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpaceEndGateActivation", &[PyRep::Integer(888_001)]).await.expect("VoidSpaceEndGateActivation");
    let void_out_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&void_out_notifs, "OnVoidSpaceContentFinished"), &[PyRep::Integer(fil_id), PyRep::Integer(1)]);

    // 3. Season challenge claim -> OnSeasonalGoalCompleted + OnSeasonOmegaRewardsClaimed (Omega only)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(JITA_STATION);
        }
    });
    sess.station_id = Some(JITA_STATION);
    rpc(&ctx, &mut sess, "seasonManager", "select_season", &[PyRep::Integer(1)]).await.expect("select_season");
    rpc(&ctx, &mut sess, "seasonManager", "process_client_event", &[PyRep::Integer(1_005), PyRep::Integer(1)]).await.expect("step 1");
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    rpc(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)]).await.expect("claim");
    let claim_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&claim_notifs, "OnSeasonalGoalCompleted")[0], PyRep::Integer(0));
    assert_eq!(find_notif(&claim_notifs, "OnSeasonOmegaRewardsClaimed"), &[PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(1)])]);
}

// parity: notify:OnRaffleCreatedServer, notify:OnRaffleUpdatedServer, notify:OnRaffleFinishedServer
// parity: notify:OnMultiEvent, notify:OnCharacterEnergyChanged
#[tokio::test]
async fn test_notify_live_raffle_multievent_energy_and_alliance() {
    eve_testkit::attest(&[
        "notify:OnRaffleCreatedServer",
        "notify:OnRaffleUpdatedServer",
        "notify:OnRaffleFinishedServer",
        "notify:OnMultiEvent",
        "notify:OnCharacterEnergyChanged",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let (ship_a, ship_b, prize_id) = (640_000_101_i64, 640_000_102_i64, 640_000_110_i64);
    let mut sess_a = login_in_space(&ctx, 9_501, "demo", "demo-secret", PILOT_A, ship_a, 621, SYS_JITA, DVec3::ZERO).await;
    let mut sess_b = login_in_space(&ctx, 9_502, "other", "other-secret", PILOT_B, ship_b, 621, SYS_JITA, DVec3::new(1_000.0, 0.0, 0.0)).await;

    // 1. AddTarget -> OnMultiEvent carrying ('OnTarget', 'add', ship_b, ...)
    rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b)]).await.expect("AddTarget");
    let sys_handle = ctx.systems.get_or_spawn(SYS_JITA);
    for _ in 0..5 {
        let _ = sys_handle.tick().await;
    }
    let target_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let multi_args = find_notif(&target_notifs, "OnMultiEvent");
    let PyRep::List(events) = &multi_args[0] else {
        panic!("expected OnMultiEvent list");
    };
    assert_eq!(events.len(), 1);
    let PyRep::Tuple(entry) = &events[0] else {
        panic!("expected OnMultiEvent tuple entry");
    };
    let PyRep::Tuple(inner) = &entry[0] else {
        panic!("expected OnTarget inner tuple");
    };
    assert_eq!(inner[0], PyRep::String("OnTarget".into()));
    assert_eq!(inner[1], PyRep::String("add".into()));
    assert_eq!(inner[2], PyRep::Integer(ship_b));

    // 2. /energy slash command -> OnCharacterEnergyChanged
    sess_a.role |= ROLE_GM_DEFAULT;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String("/energy 50".into())]).await.expect("/energy 50");
    let energy_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let energy_args = find_notif(&energy_notifs, "OnCharacterEnergyChanged");
    assert_eq!(energy_args.len(), 2);

    // 3. HyperNet raffle creation + ticket purchase -> OnRaffleCreatedServer, OnRaffleUpdatedServer, OnRaffleFinishedServer
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(JITA_STATION);
            ch.balance = IskCents::from_isk(100_000_000);
        }
        if let Some(ch) = snap.characters.get_mut(&PILOT_B) {
            ch.station_id = Some(JITA_STATION);
            ch.balance = IskCents::from_isk(100_000_000);
        }
        snap.items.insert(prize_id, make_item(prize_id, 587, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true));
        snap.items.insert(prize_id + 1, make_item(prize_id + 1, 52_568, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 10, false));
    });
    sess_a.station_id = Some(JITA_STATION);
    sess_b.station_id = Some(JITA_STATION);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    let raffle_rep = rpc(
        &ctx,
        &mut sess_a,
        "raffleProxy",
        "CreateRaffle",
        &[
            PyRep::Integer(prize_id),
            PyRep::Integer(8),
            PyRep::Integer(1_000_000),
        ],
    )
    .await
    .expect("CreateRaffle");
    let raffle_id = raffle_rep.dict_get("raffle_id").and_then(|v| v.as_i64()).expect("raffle_id");
    let create_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let created_args = find_notif(&create_notifs, "OnRaffleCreatedServer");
    assert_eq!(created_args[0], PyRep::Integer(raffle_id));
    assert_eq!(created_args[1].dict_get("raffle_id").and_then(|v| v.as_i64()), Some(raffle_id));

    rpc(&ctx, &mut sess_b, "raffleProxy", "BuyRandomTickets", &[PyRep::Integer(raffle_id), PyRep::Integer(8)])
        .await
        .expect("BuyRandomTickets");
    let buy_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    let updated_args = find_notif(&buy_notifs_b, "OnRaffleUpdatedServer");
    assert_eq!(updated_args[0], PyRep::Integer(raffle_id));
    assert_eq!(updated_args[1].dict_get("raffle_id").and_then(|v| v.as_i64()), Some(raffle_id));
    let finished_args = find_notif(&buy_notifs_b, "OnRaffleFinishedServer");
    assert_eq!(finished_args[0], PyRep::Integer(raffle_id));
}
