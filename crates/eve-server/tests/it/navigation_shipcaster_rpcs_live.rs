//! Live integration suite for Shipcaster RPCs:
//! - `rpc:shipCasterLandingPadMgr.GetAllLandingPads`
//! - `rpc:shipCasterLandingPadMgr.GetFactionLandingPads`
//! - `rpc:shipCasterLauncherMgr.GetFactionsWithShipcaster`
//! - `rpc:starterShipcasterTravelMgr.CanCharacterJump`
//! - `rpc:starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster`
//! - `rpc:shipcasterTravelMgr.CmdJumpThroughShipcaster`

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use eve_db::{InventoryFlag, VirtualServerClock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::cluster_macho_misc_services::{sde_launchers, sde_starter_launchers};
use eve_server::services::space_destiny::clear_session_change_timer;
use glam::DVec3;

use crate::common;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

fn kv_i64(rep: &PyRep, key: &str) -> Option<i64> {
    rep.dict_get(key).and_then(|v| v.as_i64())
}

fn kv_bool(rep: &PyRep, key: &str) -> Option<bool> {
    match rep.dict_get(key) {
        Some(PyRep::Bool(b)) => Some(*b),
        _ => None,
    }
}

async fn deploy_and_build_caldari_beacon_live(
    ctx: &ServerContext,
    pilot: &mut common::LivePilot,
    sys_id: u32,
) -> i64 {
    common::slash_cmd(ctx, &mut pilot.sess, &format!("/tr {sys_id}")).await;

    // 75933 = Caldari Shipcaster Beacon Constructor (underConstruction)
    let ctor_rep = common::slash_cmd(ctx, &mut pilot.sess, "/createitem 75933 1").await;
    let ctor_id = ctor_rep
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("constructor itemID");

    let drop_res = rpc(
        ctx,
        &mut pilot.sess,
        "ship",
        "Drop",
        &[
            PyRep::List(vec![PyRep::Integer(ctor_id)]),
            PyRep::Integer(pilot.char_id),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("ship.Drop constructor");

    let pad_id = match &drop_res {
        PyRep::Dict(pairs) => pairs
            .iter()
            .find(|(k, _)| k.as_i64() == Some(ctor_id))
            .and_then(|(_, v)| match v {
                PyRep::List(ids) => ids.first().and_then(|id| id.as_i64()),
                _ => None,
            })
            .expect("deployed pad id"),
        other => panic!("Unexpected Drop result: {other:?}"),
    };

    // Stock SDE-required construction materials into ship cargo and deposit via CallComponentFromClient
    let comps = ctx
        .sde
        .shipcaster_sde_components()
        .expect("shipcaster SDE components");
    let spec = comps
        .under_construction
        .get(&75933)
        .expect("75933 underConstruction spec");

    let mut deposit_pairs = Vec::new();
    for (&mat_tid, entry) in &spec.input_items {
        common::stock_ship_bay(
            ctx,
            pilot,
            mat_tid,
            entry.required_qty as i32,
            InventoryFlag::Cargo as i64,
        )
        .await;
        deposit_pairs.push((
            PyRep::Integer(i64::from(mat_tid)),
            PyRep::Integer(i64::from(entry.required_qty)),
        ));
    }

    let deposit_res = rpc(
        ctx,
        &mut pilot.sess,
        "beyonce",
        "CallComponentFromClient",
        &[
            PyRep::Integer(pad_id),
            PyRep::String("underConstruction".into()),
            PyRep::String("DepositItemsFromShip".into()),
            PyRep::Dict(deposit_pairs),
        ],
    )
    .await
    .expect("DepositItemsFromShip");

    match deposit_res {
        PyRep::Tuple(items) => {
            assert_eq!(items.first(), Some(&PyRep::Bool(true)));
        }
        other => panic!("Unexpected DepositItemsFromShip result: {other:?}"),
    }

    pad_id
}

#[tokio::test]
async fn test_shipcaster_landing_pads_and_factions_live() {
    // parity: rpc:shipCasterLandingPadMgr.GetAllLandingPads
    // parity: rpc:shipCasterLandingPadMgr.GetFactionLandingPads
    // parity: rpc:shipCasterLauncherMgr.GetFactionsWithShipcaster
    eve_testkit::attest(&[
        "rpc:shipCasterLandingPadMgr.GetAllLandingPads",
        "rpc:shipCasterLandingPadMgr.GetFactionLandingPads",
        "rpc:shipCasterLauncherMgr.GetFactionsWithShipcaster",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut pilot = common::gm_pilot_in_space(&ctx, 71_001, 92_100_001, 648, 30_000_142).await;

    // 1. GetFactionsWithShipcaster returns the 4 Empire FW factions
    let factions = rpc(&ctx, &mut pilot.sess, "shipCasterLauncherMgr", "GetFactionsWithShipcaster", &[])
        .await
        .expect("GetFactionsWithShipcaster");
    assert_eq!(
        factions,
        PyRep::List(vec![
            PyRep::Integer(500_001),
            PyRep::Integer(500_002),
            PyRep::Integer(500_003),
            PyRep::Integer(500_004),
        ])
    );

    // 2. Initially 0 empire landing pads exist
    let initial_all = rpc(&ctx, &mut pilot.sess, "shipCasterLandingPadMgr", "GetAllLandingPads", &[])
        .await
        .expect("GetAllLandingPads initial");
    assert_eq!(initial_all, PyRep::List(Vec::new()));

    let initial_caldari = rpc(
        &ctx,
        &mut pilot.sess,
        "shipCasterLandingPadMgr",
        "GetFactionLandingPads",
        &[PyRep::Integer(500_001)],
    )
    .await
    .expect("GetFactionLandingPads initial");
    assert_eq!(initial_caldari, PyRep::List(Vec::new()));

    // 3. Enlist in Caldari State militia and deploy + build a beacon in Onatoh (30002811)
    rpc(
        &ctx,
        &mut pilot.sess,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_001)],
    )
    .await
    .expect("CreateMyDirectEnlistment");

    let pad_id = deploy_and_build_caldari_beacon_live(&ctx, &mut pilot, 30_002_811).await;

    // 4. Verify GetAllLandingPads and GetFactionLandingPads(500001) return the built, linked pad
    let all_pads = match rpc(&ctx, &mut pilot.sess, "shipCasterLandingPadMgr", "GetAllLandingPads", &[])
        .await
        .expect("GetAllLandingPads after build")
    {
        PyRep::List(l) => l,
        other => panic!("Expected list, got {other:?}"),
    };
    let found_all = all_pads
        .iter()
        .find(|p| kv_i64(p, "itemID") == Some(pad_id))
        .expect("Built pad in GetAllLandingPads");
    assert_eq!(kv_i64(found_all, "solarSystemID"), Some(30_002_811));
    assert_eq!(kv_i64(found_all, "factionID"), Some(500_001));
    assert_eq!(kv_bool(found_all, "isBuilt"), Some(true));
    assert_eq!(kv_bool(found_all, "isLinked"), Some(true));
    assert_eq!(kv_bool(found_all, "canBeLinked"), Some(true));

    let caldari_pads = match rpc(
        &ctx,
        &mut pilot.sess,
        "shipCasterLandingPadMgr",
        "GetFactionLandingPads",
        &[PyRep::Integer(500_001)],
    )
    .await
    .expect("GetFactionLandingPads(500001)")
    {
        PyRep::List(l) => l,
        other => panic!("Expected list, got {other:?}"),
    };
    assert_eq!(caldari_pads.len(), 1);
    assert_eq!(kv_i64(&caldari_pads[0], "itemID"), Some(pad_id));

    let minmatar_pads = rpc(
        &ctx,
        &mut pilot.sess,
        "shipCasterLandingPadMgr",
        "GetFactionLandingPads",
        &[PyRep::Integer(500_002)],
    )
    .await
    .expect("GetFactionLandingPads(500002)");
    assert_eq!(minmatar_pads, PyRep::List(Vec::new()));
}

#[tokio::test]
async fn test_starter_shipcaster_eligibility_and_jump_live() {
    // parity: rpc:starterShipcasterTravelMgr.CanCharacterJump
    // parity: rpc:starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster
    eve_testkit::attest(&[
        "rpc:starterShipcasterTravelMgr.CanCharacterJump",
        "rpc:starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let starter_launchers = sde_starter_launchers(&ctx.sde);
    let jouvulen_launcher = starter_launchers
        .iter()
        .find(|l| l.solar_system_id == 30_030_141)
        .expect("Jouvulen starter shipcaster");
    let launcher_id = jouvulen_launcher.item_id;
    let launcher_pos = jouvulen_launcher.position;
    let war_hq_sys = jouvulen_launcher.staging_system_id; // 30045324 Onnamon

    // 1. Veteran character (> 30 days old under pinned clock) is ineligible
    let vet_char_id = 92_200_001;
    ctx.db
        .provision_gm_account("vet_sc_live", "vet-pass", vet_char_id, "Veteran SC Live");
    let mut vet_sess = SessionState::new(71_002);
    common::login_pilot(&ctx, &mut vet_sess, "vet_sc_live", "vet-pass").await;
    rpc(
        &ctx,
        &mut vet_sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(vet_char_id)],
    )
    .await
    .expect("SelectCharacterID veteran");

    let vet_can = rpc(&ctx, &mut vet_sess, "starterShipcasterTravelMgr", "CanCharacterJump", &[])
        .await
        .expect("CanCharacterJump veteran");
    assert_eq!(vet_can, PyRep::Bool(false));

    let docked_err = rpc(
        &ctx,
        &mut vet_sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(launcher_id)],
    )
    .await
    .expect_err("Docked jump must fail");
    assert_eq!(docked_err.code, "CantDoThatWhileDocked");

    rpc(&ctx, &mut vet_sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock veteran");
    common::slash_cmd(&ctx, &mut vet_sess, "/tr 30030141").await;
    let vet_err = rpc(
        &ctx,
        &mut vet_sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(launcher_id)],
    )
    .await
    .expect_err("Veteran jump must fail");
    assert_eq!(vet_err.code, "CharacterTooOldForStarterShipcaster");

    // 2. Rookie character (5 days old <= 30 days) is eligible and jumps once
    let rookie_char_id = 92_200_002;
    ctx.db
        .provision_gm_account("rookie_sc_live", "rookie-pass", rookie_char_id, "Rookie SC Live");
    let created_unix = ctx
        .db
        .get_character(rookie_char_id)
        .expect("rookie char")
        .create_date_unix;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(
        created_unix + 5 * 86_400,
    )));

    let mut rookie_sess = SessionState::new(71_003);
    common::login_pilot(&ctx, &mut rookie_sess, "rookie_sc_live", "rookie-pass").await;
    rpc(
        &ctx,
        &mut rookie_sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(rookie_char_id)],
    )
    .await
    .expect("SelectCharacterID rookie");

    let ship_id = rookie_sess.ship_id.expect("rookie ship_id");
    rpc(&ctx, &mut rookie_sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock rookie");
    common::slash_cmd(&ctx, &mut rookie_sess, "/tr 30030141").await;
    rookie_sess.ship_id = Some(ship_id);

    let rookie_can = rpc(
        &ctx,
        &mut rookie_sess,
        "starterShipcasterTravelMgr",
        "CanCharacterJump",
        &[],
    )
    .await
    .expect("CanCharacterJump rookie");
    assert_eq!(rookie_can, PyRep::Bool(true));

    let jouvulen_handle = ctx.systems.get_or_spawn(30_030_141);
    jouvulen_handle
        .set_ball_position(ship_id, launcher_pos + DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    clear_session_change_timer(&ctx, rookie_char_id);

    let jump_res = rpc(
        &ctx,
        &mut rookie_sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(launcher_id)],
    )
    .await
    .expect("CmdJumpThroughStarterShipcaster rookie");
    assert_eq!(jump_res, PyRep::None);
    assert_eq!(rookie_sess.solar_system_id, Some(war_hq_sys));

    // 3. One-time-use enforcement
    let after_can = rpc(
        &ctx,
        &mut rookie_sess,
        "starterShipcasterTravelMgr",
        "CanCharacterJump",
        &[],
    )
    .await
    .expect("CanCharacterJump after jump");
    assert_eq!(after_can, PyRep::Bool(false));

    common::slash_cmd(&ctx, &mut rookie_sess, "/tr 30030141").await;
    jouvulen_handle
        .set_ball_position(ship_id, launcher_pos + DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    clear_session_change_timer(&ctx, rookie_char_id);
    let second_err = rpc(
        &ctx,
        &mut rookie_sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(launcher_id)],
    )
    .await
    .expect_err("Second starter shipcaster jump must fail");
    assert_eq!(second_err.code, "StarterShipcasterAlreadyUsed");
}

#[tokio::test]
async fn test_interstellar_shipcaster_war_hq_and_zarzakh_jump_live() {
    // parity: rpc:shipcasterTravelMgr.CmdJumpThroughShipcaster
    eve_testkit::attest(&["rpc:shipcasterTravelMgr.CmdJumpThroughShipcaster"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut pilot = common::gm_pilot_in_space(&ctx, 71_004, 92_300_001, 648, 30_000_142).await;

    // 1. Enlist in Caldari State militia and build a Caldari FW landing pad in Onatoh (30002811)
    rpc(
        &ctx,
        &mut pilot.sess,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_001)],
    )
    .await
    .expect("CreateMyDirectEnlistment");

    let pad_id = deploy_and_build_caldari_beacon_live(&ctx, &mut pilot, 30_002_811).await;

    // 2. Travel to Onnamon War HQ (30045324) and jump through Caldari Interstellar Shipcaster
    let launchers = sde_launchers(&ctx.sde);
    let caldari_launcher = launchers
        .iter()
        .find(|l| l.faction_id == 500_001)
        .expect("Caldari War HQ launcher");
    let war_hq_sys = caldari_launcher.solar_system_id;
    assert_eq!(war_hq_sys, 30_045_324);

    common::slash_cmd(&ctx, &mut pilot.sess, &format!("/tr {war_hq_sys}")).await;
    let war_hq_handle = ctx.systems.get_or_spawn(war_hq_sys);
    war_hq_handle
        .set_ball_position(
            pilot.ship_id,
            caldari_launcher.position + DVec3::new(1_000.0, 0.0, 0.0),
        )
        .await;
    clear_session_change_timer(&ctx, pilot.char_id);

    let jump_ok = rpc(
        &ctx,
        &mut pilot.sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[
            PyRep::Integer(caldari_launcher.item_id),
            PyRep::Integer(30_002_811),
            PyRep::Integer(pad_id),
        ],
    )
    .await
    .expect("CmdJumpThroughShipcaster War HQ -> Onatoh");
    assert_eq!(jump_ok, PyRep::None);
    assert_eq!(pilot.sess.solar_system_id, Some(30_002_811));

    // 3. Deathless Shipcaster in Turnur (30002086) refuses Empire militia, allows neutral pilot to Zarzakh
    let turnur_launcher = launchers
        .iter()
        .find(|l| l.solar_system_id == 30_002_086 && l.faction_id == 0)
        .expect("Turnur Deathless entry beacon");
    let zarzakh_pad_id = turnur_launcher.linked_pad_ids[0];

    common::slash_cmd(&ctx, &mut pilot.sess, "/tr 30002086").await;
    let turnur_handle = ctx.systems.get_or_spawn(30_002_086);
    turnur_handle
        .set_ball_position(
            pilot.ship_id,
            turnur_launcher.position + DVec3::new(1_000.0, 0.0, 0.0),
        )
        .await;
    clear_session_change_timer(&ctx, pilot.char_id);

    let refused = rpc(
        &ctx,
        &mut pilot.sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[
            PyRep::Integer(turnur_launcher.item_id),
            PyRep::Integer(30_100_000),
            PyRep::Integer(zarzakh_pad_id),
        ],
    )
    .await
    .expect_err("Empire militia must be refused by Deathless Shipcaster");
    assert_eq!(refused.code, "EmpireMilitiaRefused");

    rpc(
        &ctx,
        &mut pilot.sess,
        "fwCharacterEnlistmentMgr",
        "RemoveMyDirectEnlistment",
        &[],
    )
    .await
    .expect("RemoveMyDirectEnlistment");

    clear_session_change_timer(&ctx, pilot.char_id);
    let zarzakh_ok = rpc(
        &ctx,
        &mut pilot.sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[
            PyRep::Integer(turnur_launcher.item_id),
            PyRep::Integer(30_100_000),
            PyRep::Integer(zarzakh_pad_id),
        ],
    )
    .await
    .expect("Neutral pilot jump to Zarzakh");
    assert_eq!(zarzakh_ok, PyRep::None);
    assert_eq!(pilot.sess.solar_system_id, Some(30_100_000));
}
