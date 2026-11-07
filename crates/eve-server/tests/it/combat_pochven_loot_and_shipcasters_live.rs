//! Live combat simulation and shipcaster navigation test suite:
//! - `feat:combat.pochven-loot-drop`: Live PvP ship destruction in Triglavian Pochven space (Otela 30000157,
//!   region 10000070) with 60% baseline loot drop threshold vs 50% in standard empire space.
//! - `feat:navigation.starter-shipcaster`: Rookie character age eligibility, docked rejection, live jump
//!   through starter shipcaster launcher to empire War HQ, and strict one-time-use enforcement.
//! - `feat:navigation.shipcaster`: Interstellar shipcaster travel, militia refusal checks, and live jump
//!   through Turnur entry beacon to Zarzakh.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::cluster_macho_misc_services::sde_starter_launchers;
use eve_server::services::security_mgr::insurgency_pvp_loot_drop_threshold;
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

#[tokio::test]
async fn test_combat_pochven_loot_drop_live() {
    // parity: feat:combat.pochven-loot-drop
    eve_testkit::attest(&["feat:combat.pochven-loot-drop"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // Otela (30000157) is located in Triglavian Pochven space (region 10000070).
    // Jita (30000142) is located in standard empire space (The Forge, region 10000002).
    let pochven_sys = 30_000_157;
    let empire_sys = 30_000_142;
    let killer_char_id = 91_300_001;
    let victim_char_id = 91_300_002;

    // 1. Verify baseline published loot drop threshold:
    // Pochven systems have a baseline 0.60 (60%) loot drop rate, compared to 0.50 (50%) in standard empire space.
    let pochven_drop_rate =
        insurgency_pvp_loot_drop_threshold(&ctx, pochven_sys, killer_char_id, victim_char_id, false);
    assert_eq!(
        pochven_drop_rate, 0.60,
        "Pochven loot drop threshold must be 60% (0.60)"
    );

    let empire_drop_rate =
        insurgency_pvp_loot_drop_threshold(&ctx, empire_sys, killer_char_id, victim_char_id, false);
    assert_eq!(
        empire_drop_rate, 0.50,
        "Empire loot drop threshold must be 50% (0.50)"
    );

    // 2. Fly both pilots into space in Otela (Pochven) via live GM provisioning and RPCs
    let mut killer = common::gm_pilot_in_space(&ctx, 50_001, killer_char_id, 587, pochven_sys).await; // Rifter
    let mut victim = common::gm_pilot_in_space(&ctx, 50_002, victim_char_id, 601, pochven_sys).await; // Ibis

    // 3. Populate victim cargo with 20 distinct item stacks in space using live slash commands
    let cargo_type_ids = [
        34, 35, 36, 37, 38, 39, 40, 185, 2881, 16275,
        3402, 3412, 3413, 3426, 3300, 3303, 3306, 587, 601, 75933,
    ];
    for &tid in &cargo_type_ids {
        common::slash_cmd(&ctx, &mut victim.sess, &format!("/createitem {tid} 10")).await;
    }

    let initial_cargo_items: Vec<_> = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|i| i.location_id == victim.ship_id && i.flag == InventoryFlag::Cargo)
            .cloned()
            .collect()
    });
    assert!(
        initial_cargo_items.len() >= cargo_type_ids.len(),
        "Victim ship cargo must contain seeded items in space"
    );

    // 4. Arm killer with 3x 200mm AutoCannon I and EMP S ammunition
    for sk in [3300_u32, 3303, 3306, 11083, 3413, 3426] {
        common::slash_cmd(&ctx, &mut killer.sess, &format!("/giveskill {sk} 5")).await;
    }
    let mut guns = Vec::new();
    for _ in 0..3 {
        let rep = common::slash_cmd(&ctx, &mut killer.sess, "/fit 2881").await;
        let gid = rep.dict_get("itemID").expect("gun itemID").as_i64().expect("gun i64");
        guns.push(gid);
    }
    let ammo_rep = common::slash_cmd(&ctx, &mut killer.sess, "/createitem 185 2000").await;
    let ammo_id = ammo_rep.dict_get("itemID").expect("ammo itemID").as_i64().expect("ammo i64");
    let gun_reps: Vec<PyRep> = guns.iter().map(|&g| PyRep::Integer(g)).collect();
    rpc(&ctx, &mut killer.sess, "dogmaIM", "LoadAmmo", &[
        PyRep::Integer(killer.ship_id),
        PyRep::List(gun_reps),
        PyRep::List(vec![PyRep::Integer(ammo_id)]),
    ])
    .await
    .expect("LoadAmmo");

    // 5. Position ships on grid in Pochven space
    let pochven_handle = ctx.systems.get_or_spawn(pochven_sys);
    pochven_handle
        .set_ball_position(victim.ship_id, DVec3::new(10_000.0, 0.0, 0.0))
        .await;
    pochven_handle
        .set_ball_position(killer.ship_id, DVec3::new(11_000.0, 0.0, 0.0))
        .await;

    // 6. Lock victim ship via dogmaIM RPC
    rpc(
        &ctx,
        &mut killer.sess,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(victim.ship_id)],
    )
    .await
    .expect("dogmaIM.AddTarget");
    for _ in 0..30 {
        pochven_handle.step_manual(1.0).await.unwrap();
        if let Ok(locks) = pochven_handle.get_target_locks(killer.ship_id).await {
            if locks.iter().any(|l| l.target_id == victim.ship_id && l.locked) {
                break;
            }
        }
    }

    // 7. Drive live weapons destruction via dogmaIM.Activate and simulation manual ticks
    let mut ship_destroyed = false;
    for _ in 0..50 {
        if pochven_handle
            .get_ball_snapshot(victim.ship_id)
            .await
            .unwrap()
            .is_none()
        {
            ship_destroyed = true;
            break;
        }
        for &gid in &guns {
            let _ = rpc(
                &ctx,
                &mut killer.sess,
                "dogmaIM",
                "Activate",
                &[
                    PyRep::Integer(gid),
                    PyRep::String("targetAttack".into()),
                    PyRep::Integer(victim.ship_id),
                ],
            )
            .await;
        }
        for _ in 0..5 {
            pochven_handle.step_manual(1.0).await.unwrap();
        }
        ctx.advance_secs(5);
    }
    assert!(
        ship_destroyed
            || pochven_handle
                .get_ball_snapshot(victim.ship_id)
                .await
                .unwrap()
                .is_none(),
        "Victim ship must be destroyed by live weapon fire"
    );

    // 8. Inspect wreck spawned in space at Pochven system (exclude victim's ejected capsule type 670)
    let wreck = ctx
        .db
        .with_snapshot(|s| {
            s.items
                .values()
                .find(|i| {
                    i.location_id == pochven_sys as i64
                        && i.owner_id == victim_char_id
                        && i.type_id != 670
                })
                .cloned()
        })
        .expect("Wreck container item must exist in space in Pochven");

    let dropped_in_wreck: Vec<_> = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|i| i.location_id == wreck.item_id)
            .cloned()
            .collect()
    });
    assert!(
        !dropped_in_wreck.is_empty(),
        "Pochven wreck must contain dropped loot items"
    );

    // 9. Inspect live killmail generated by death pipeline
    let km = ctx
        .db
        .with_snapshot(|s| {
            s.killmails
                .values()
                .find(|k| {
                    k.victim_character_id == victim_char_id && k.solar_system_id == pochven_sys
                })
                .cloned()
        })
        .expect("Killmail must be generated in Pochven");

    assert_eq!(km.solar_system_id, pochven_sys);
    assert_eq!(km.victim_character_id, victim_char_id);
    assert_eq!(km.attacker_character_id, killer_char_id);
    assert!(km.total_value_isk > 0.0);

    // Verify dropped loot vs destroyed cargo:
    // With 20 cargo items and 60% Pochven drop rate, some items drop into wreck and some are destroyed
    assert!(
        dropped_in_wreck.len() > 0 && dropped_in_wreck.len() < initial_cargo_items.len(),
        "Pochven destruction must drop a subset of items into wreck container (got {} dropped out of {})",
        dropped_in_wreck.len(),
        initial_cargo_items.len()
    );
}

#[tokio::test]
async fn test_navigation_starter_and_interstellar_shipcasters_live() {
    // parity: feat:navigation.starter-shipcaster
    // parity: feat:navigation.shipcaster
    eve_testkit::attest(&[
        "feat:navigation.starter-shipcaster",
        "feat:navigation.shipcaster",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // =========================================================================
    // Part 1: Starter Shipcaster (feat:navigation.starter-shipcaster)
    // =========================================================================
    // 1. Verify SDE starter launchers exist across >= 64 starter/career systems
    let sde_launchers = sde_starter_launchers(&ctx.sde);
    assert!(
        sde_launchers.len() >= 64,
        "Expected >= 64 starter shipcaster launchers in SDE"
    );
    let jouvulen_launcher = sde_launchers
        .iter()
        .find(|l| l.solar_system_id == 30030141) // Jouvulen (Caldari career system)
        .expect("Jouvulen starter shipcaster");
    let jouvulen_launcher_id = jouvulen_launcher.item_id;
    let jouvulen_launcher_pos = jouvulen_launcher.position;
    assert_eq!(jouvulen_launcher.staging_system_id, 30045324); // Onnamon War HQ

    // 2. Veteran character (> 30 days old) rejection:
    // Character provisioned with default create_date_unix (1704067200) is ~3 years old under pinned clock (1800000000)
    let vet_char_id = 91_400_001;
    let vet_user = "vet_shipcaster_pilot";
    let vet_pass = "vet-secret-pass";
    ctx.db.provision_gm_account(vet_user, vet_pass, vet_char_id, "Veteran Shipcaster Pilot");

    let mut vet_sess = SessionState::new(60_001);
    common::login_pilot(&ctx, &mut vet_sess, vet_user, vet_pass).await;
    rpc(
        &ctx,
        &mut vet_sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(vet_char_id)],
    )
    .await
    .expect("SelectCharacterID veteran");

    let vet_can_jump = rpc(
        &ctx,
        &mut vet_sess,
        "starterShipcasterTravelMgr",
        "CanCharacterJump",
        &[],
    )
    .await
    .expect("CanCharacterJump veteran");
    assert_eq!(
        vet_can_jump,
        PyRep::Bool(false),
        "Veteran (>30 days) must not be eligible"
    );

    let docked_vet_err = rpc(
        &ctx,
        &mut vet_sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(jouvulen_launcher_id)],
    )
    .await
    .expect_err("Docked veteran jump must fail with CantDoThatWhileDocked");
    assert_eq!(docked_vet_err.code, "CantDoThatWhileDocked");

    // Undock veteran into space: now age check fires
    rpc(&ctx, &mut vet_sess, "stationSvc", "Undock", &[])
        .await
        .expect("stationSvc.Undock veteran");
    common::slash_cmd(&ctx, &mut vet_sess, "/tr 30030141").await;

    let vet_jump_err = rpc(
        &ctx,
        &mut vet_sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(jouvulen_launcher_id)],
    )
    .await
    .expect_err("Veteran jump must be rejected");
    assert_eq!(vet_jump_err.code, "CharacterTooOldForStarterShipcaster");

    // 3. Rookie pilot eligibility & live jump:
    // Provision fresh rookie pilot with create_date_unix <= 30 days old by setting clock to 5 days after creation
    let rookie_char_id = 90_000_003;
    let user_name = "rookie_shipcaster_pilot";
    let user_pass = "rookie-secret-pass";
    ctx.db.provision_gm_account(user_name, user_pass, rookie_char_id, "Rookie Shipcaster Pilot");

    // Pin clock to 5 days after rookie character creation date (1704067200 + 5 * 86400)
    let rookie_created_unix = 1704067200;
    ctx.set_clock(std::sync::Arc::new(
        eve_db::VirtualServerClock::with_fixed_base(rookie_created_unix + 5 * 86_400),
    ));

    let mut rookie_sess = SessionState::new(60_002);
    common::login_pilot(&ctx, &mut rookie_sess, user_name, user_pass).await;
    rpc(
        &ctx,
        &mut rookie_sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(rookie_char_id)],
    )
    .await
    .expect("SelectCharacterID rookie");

    // Docked check: cannot jump while docked in station
    let docked_err = rpc(
        &ctx,
        &mut rookie_sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(jouvulen_launcher_id)],
    )
    .await
    .expect_err("Docked jump must fail");
    assert_eq!(docked_err.code, "CantDoThatWhileDocked");

    // Undock and teleport to Jouvulen career system
    let ship_id = rookie_sess.ship_id.unwrap();
    rpc(&ctx, &mut rookie_sess, "stationSvc", "Undock", &[])
        .await
        .expect("stationSvc.Undock");
    common::slash_cmd(&ctx, &mut rookie_sess, "/tr 30030141").await;
    rookie_sess.ship_id = Some(ship_id);

    // Now undocked and rookie (5 days old <= 30 days): CanCharacterJump must return true!
    let rookie_can_jump = rpc(
        &ctx,
        &mut rookie_sess,
        "starterShipcasterTravelMgr",
        "CanCharacterJump",
        &[],
    )
    .await
    .expect("CanCharacterJump rookie in space");
    assert_eq!(
        rookie_can_jump,
        PyRep::Bool(true),
        "Eligible rookie must be allowed to jump"
    );

    // Move rookie within 1,000 m of the Jouvulen starter launcher (within SHIPCASTER_MAX_RANGE_M = 5,000 m)
    let jouvulen_handle = ctx.systems.get_or_spawn(30030141);
    jouvulen_handle
        .set_ball_position(ship_id, jouvulen_launcher_pos + DVec3::new(1_000.0, 0.0, 0.0))
        .await;

    // Clear session change cooldown before jump
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, rookie_char_id);

    // Live jump through starter shipcaster
    let jump_res = rpc(
        &ctx,
        &mut rookie_sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(jouvulen_launcher_id)],
    )
    .await
    .expect("CmdJumpThroughStarterShipcaster must succeed for rookie");
    assert_eq!(jump_res, PyRep::None);

    // Verify session updated to Onnamon War HQ (30045324)
    assert_eq!(
        rookie_sess.solar_system_id,
        Some(30045324),
        "Pilot must arrive in Onnamon War HQ"
    );

    // One-time-use rule: CanCharacterJump immediately flips to false
    let after_jump_can = rpc(
        &ctx,
        &mut rookie_sess,
        "starterShipcasterTravelMgr",
        "CanCharacterJump",
        &[],
    )
    .await
    .expect("CanCharacterJump after jump");
    assert_eq!(
        after_jump_can,
        PyRep::Bool(false),
        "Starter shipcaster is strictly one-time-use per character"
    );

    // Second jump attempt fails with StarterShipcasterAlreadyUsed
    common::slash_cmd(&ctx, &mut rookie_sess, "/tr 30030141").await;
    jouvulen_handle
        .set_ball_position(ship_id, jouvulen_launcher_pos + DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, rookie_char_id);
    let second_jump_err = rpc(
        &ctx,
        &mut rookie_sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(jouvulen_launcher_id)],
    )
    .await
    .expect_err("Second jump through starter shipcaster must fail");
    assert_eq!(second_jump_err.code, "StarterShipcasterAlreadyUsed");

    // =========================================================================
    // Part 2: Interstellar Shipcaster (feat:navigation.shipcaster)
    // =========================================================================
    // Deathless Interstellar Shipcaster entry beacon in Turnur (30002086)
    // Launcher item ID: 105_000_011 (pos: 250,000, 0, 0)
    // Target landing pad: 104_000_030 in Zarzakh (30100000)
    let turnur_sys = 30_002_086;
    let zarzakh_sys = 30_100_000;
    let zarzakh_entry_launcher_id = 105_000_011;
    let zarzakh_pad_id = 104_000_030;
    let launcher_pos = DVec3::new(250_000.0, 0.0, 0.0);

    // Teleport rookie to Turnur and position within 1,000 m of beacon
    common::slash_cmd(&ctx, &mut rookie_sess, &format!("/tr {turnur_sys}")).await;
    let turnur_handle = ctx.systems.get_or_spawn(turnur_sys);
    turnur_handle
        .set_ball_position(ship_id, launcher_pos + DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, rookie_char_id);

    // Negative check: Empire militia enlisted pilot is refused by Deathless Shipcaster (enlist while docked, then undock)
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&rookie_char_id).unwrap().station_id = Some(60_003_760); });
    rookie_sess.station_id = Some(60_003_760);
    rpc(
        &ctx,
        &mut rookie_sess,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_001)],
    )
    .await
    .expect("Enlist in Caldari State militia");
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&rookie_char_id).unwrap().station_id = None; });
    rookie_sess.station_id = None;
    let empire_refused = rpc(
        &ctx,
        &mut rookie_sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[
            PyRep::Integer(zarzakh_entry_launcher_id),
            PyRep::Integer(zarzakh_sys as i64),
            PyRep::Integer(zarzakh_pad_id),
        ],
    )
    .await
    .expect_err("Empire militia must be refused by Deathless Shipcaster");
    assert_eq!(empire_refused.code, "EmpireMilitiaRefused");

    // Remove direct enlistment while docked, then return to grid as neutral
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&rookie_char_id).unwrap().station_id = Some(60_003_760); });
    rookie_sess.station_id = Some(60_003_760);
    rpc(
        &ctx,
        &mut rookie_sess,
        "fwCharacterEnlistmentMgr",
        "RemoveMyDirectEnlistment",
        &[],
    )
    .await
    .expect("RemoveMyDirectEnlistment");
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&rookie_char_id).unwrap().station_id = None; });
    rookie_sess.station_id = None;

    // Positive check: Neutral pilot jumps through Turnur beacon into Zarzakh
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, rookie_char_id);
    let zarzakh_jump_ok = rpc(
        &ctx,
        &mut rookie_sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[
            PyRep::Integer(zarzakh_entry_launcher_id),
            PyRep::Integer(zarzakh_sys as i64),
            PyRep::Integer(zarzakh_pad_id),
        ],
    )
    .await
    .expect("Neutral pilot jump through Turnur entry beacon to Zarzakh");
    assert_eq!(zarzakh_jump_ok, PyRep::None);

    // Verify arrival in Zarzakh
    assert_eq!(
        rookie_sess.solar_system_id,
        Some(zarzakh_sys),
        "Pilot must arrive in Zarzakh"
    );
}
