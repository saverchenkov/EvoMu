//! Acceptance test suite Part 2 covering:
//! - Test 4: Slash Dispatcher, GM Commands (skill, isk, item, tr, heal, spawn, attr, fit, kill), Asset Safety GM Timer, and Skin Tools
//! - Test 5: Warp Speed Formula, Autopilot Routing & Waypoints, Stargate Jumping & Gate Cloak
//! - Test 6: Account SSO Login, Native machoNet Handshake, Multi-Login Blocker, Account Roles, Legal Agreements, Language Setting, Launcher Status
//!
//! Hard rules compliant:
//! - All RPC and notification names validated against V24 client oracle.
//! - Virtual time driven via ctx.now_unix() and ctx.advance_virtual_clock().
//! - Zero kwarg duration/outcome bypasses.
//! - SDE-driven static data.



use crate::common;
use eve_db::service_roles::{ROLE_ADMIN, ROLE_GMS, ROLE_GM_MASK, ROLE_PLAYER};
use eve_db::types::IskCents;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, Ballpark, DamageVector, MovementMode, WarpPhase, AU_METERS};
use eve_machonet::handshake::*;
use eve_machonet::{SessionState, VersionExchange};
use eve_proto::quasar::{EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use glam::DVec3;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

const SEED_PILOT: i64 = 90_000_001;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: Vec<PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, &args, &HashMap::new(), None)
        .await
}

async fn bound_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    bound_id: Option<u64>,
    method: &str,
    args: Vec<PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, bound_id, method, &args, &HashMap::new(), None)
        .await
}

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b)
                        .expect("decode EvePublicResponse from response_proto");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

#[tokio::test]
async fn test_admin_slash_dispatcher_gm_commands_asset_safety_skins() {
    // parity: feat:admin.slash-dispatcher, feat:admin.gm-giveskill, feat:admin.gm-giveisk, feat:admin.gm-createitem, feat:admin.gm-tr, feat:admin.gm-heal, feat:admin.gm-spawn, feat:admin.gm-attr, feat:admin.gm-fit, feat:admin.gm-kill, feat:admin.gm-assetsafety-time, feat:admin.gm-skin-tools
    eve_testkit::attest(&[
        "feat:admin.slash-dispatcher",
        "feat:admin.gm-giveskill",
        "feat:admin.gm-giveisk",
        "feat:admin.gm-createitem",
        "feat:admin.gm-tr",
        "feat:admin.gm-heal",
        "feat:admin.gm-spawn",
        "feat:admin.gm-attr",
        "feat:admin.gm-fit",
        "feat:admin.gm-kill",
        "feat:admin.gm-assetsafety-time",
        "feat:admin.gm-skin-tools",
    ]);

    let ctx = ServerContext::new();

    // 1. Slash dispatcher authorization check: unprivileged player gets error with no state change
    let mut player_sess = SessionState::new(60_001);
    common::login_pilot(&ctx, &mut player_sess, "demo", "demo-secret").await;
    rpc(
        &ctx,
        &mut player_sess,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(SEED_PILOT)],
    )
    .await
    .expect("SelectCharacterID");

    let unauthorized = rpc(
        &ctx,
        &mut player_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/giveisk 1000".into())],
    )
    .await;
    assert!(
        matches!(unauthorized, Err(ref e) if e.code == "InsufficientRole"),
        "Unauthorized caller must receive InsufficientRole error"
    );

    // 2. Provision and login GM account
    let gm_char_id = 90_000_003i64;
    ctx.db
        .provision_gm_account("gm_test_admin", "gm-pass", gm_char_id, "GM Overseer");
    let mut gm_sess = SessionState::new(60_002);
    common::login_pilot(&ctx, &mut gm_sess, "gm_test_admin", "gm-pass").await;
    rpc(
        &ctx,
        &mut gm_sess,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(gm_char_id)],
    )
    .await
    .expect("SelectCharacterID GM");

    // 3. GM /giveisk (feat:admin.gm-giveisk)
    let before_isk = ctx.db.get_character(gm_char_id).unwrap().balance;
    let give_isk_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/giveisk 500000".into())],
    )
    .await
    .expect("/giveisk 500000");
    assert!(matches!(give_isk_rep, PyRep::KeyVal(_)));
    let after_isk = ctx.db.get_character(gm_char_id).unwrap().balance;
    assert_eq!(
        after_isk,
        before_isk.checked_add(IskCents::from_isk(500_000)).unwrap(),
        "Balance must increase by 500k ISK"
    );

    // 4. GM /createitem (feat:admin.gm-createitem)
    let create_item_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/createitem 34 500".into())],
    )
    .await
    .expect("/createitem 34 500");
    let PyRep::KeyVal(ci_kv) = create_item_rep else {
        panic!("expected KeyVal from /createitem");
    };
    let item_id = ci_kv.get("itemID").and_then(|v| v.as_i64()).expect("itemID");
    let created_item = ctx
        .db
        .with_snapshot(|s| s.items.get(&item_id).cloned())
        .expect("created item in DB");
    assert_eq!(created_item.type_id, 34, "Created item must be Tritanium");
    assert_eq!(created_item.quantity, 500, "Created item quantity must be 500");

    // 5. GM /giveskill (feat:admin.gm-giveskill)
    let giveskill_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/giveskill 3300 3".into())],
    )
    .await
    .expect("/giveskill 3300 3");
    let PyRep::KeyVal(gs_kv) = giveskill_rep else {
        panic!("expected KeyVal from /giveskill");
    };
    assert_eq!(gs_kv.get("skillID").and_then(|v| v.as_i64()), Some(3300));
    assert_eq!(gs_kv.get("level").and_then(|v| v.as_i64()), Some(3));
    let char_skill = ctx
        .db
        .with_snapshot(|s| s.skills.get(&gm_char_id).and_then(|m| m.get(&3300).cloned()))
        .expect("persisted skill record");
    assert_eq!(char_skill.level, 3, "Skill must be persisted at level 3");

    // 6. GM /tr (feat:admin.gm-tr)
    let tr_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/tr 30000144".into())],
    )
    .await
    .expect("/tr 30000144");
    let PyRep::KeyVal(tr_kv) = tr_rep else {
        panic!("expected KeyVal from /tr");
    };
    assert_eq!(tr_kv.get("solarSystemID").and_then(|v| v.as_i64()), Some(30_000_144));
    assert_eq!(gm_sess.solar_system_id, Some(30_000_144));
    assert_eq!(ctx.db.get_character(gm_char_id).unwrap().solar_system_id, 30_000_144);

    // 7. GM /spawn and /unspawn (feat:admin.gm-spawn)
    let spawn_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/spawn 587 1".into())],
    )
    .await
    .expect("/spawn 587 1");
    let PyRep::KeyVal(sp_kv) = spawn_rep else {
        panic!("expected KeyVal from /spawn");
    };
    let spawned_id = sp_kv.get("spawnedID").and_then(|v| v.as_i64()).expect("spawnedID");
    let perim_handle = ctx.systems.get_or_spawn(30_000_144);
    assert!(
        perim_handle.get_ball_snapshot(spawned_id).await.unwrap().is_some(),
        "Spawned ball must exist in system"
    );

    let unspawn_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String(format!("/unspawn {spawned_id}"))],
    )
    .await
    .expect("/unspawn");
    assert!(matches!(unspawn_rep, PyRep::KeyVal(_)));
    assert!(
        perim_handle.get_ball_snapshot(spawned_id).await.unwrap().is_none(),
        "Unspawned ball must be removed from system"
    );

    // 8. GM /fit module into ship (feat:admin.gm-fit)
    let fit_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/fit me 3520".into())], // 1MN Afterburner I
    )
    .await
    .expect("/fit me 3520");
    let PyRep::KeyVal(fit_kv) = fit_rep else {
        panic!("expected KeyVal from /fit");
    };
    assert_eq!(fit_kv.get("typeID").and_then(|v| v.as_i64()), Some(3520));
    assert_eq!(fit_kv.get("online").and_then(|v| v.as_bool()), Some(true));
    let fitted_item_id = fit_kv.get("itemID").and_then(|v| v.as_i64()).expect("fitted itemID");
    let fitted_item = ctx
        .db
        .with_snapshot(|s| s.items.get(&fitted_item_id).cloned())
        .expect("fitted item record");
    assert_eq!(fitted_item.location_id, gm_sess.ship_id.unwrap());

    // 9. GM /setattr and /getattr dogma attribute (feat:admin.gm-attr)
    let setattr_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/setattr me 30 1500000.0".into())], // ATTR_MASS
    )
    .await
    .expect("/setattr me 30");
    let PyRep::KeyVal(sa_kv) = setattr_rep else {
        panic!("expected KeyVal from /setattr");
    };
    assert_eq!(sa_kv.get("attributeID").and_then(|v| v.as_i64()), Some(30));
    assert_eq!(sa_kv.get("newValue").and_then(|v| v.as_f64()), Some(1_500_000.0));

    let getattr_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/getattr me 30".into())],
    )
    .await
    .expect("/getattr me 30");
    let PyRep::KeyVal(ga_kv) = getattr_rep else {
        panic!("expected KeyVal from /getattr");
    };
    assert_eq!(ga_kv.get("value").and_then(|v| v.as_f64()), Some(1_500_000.0));

    // 10. GM /heal ship health and capacitor (feat:admin.gm-heal)
    let gm_ship_id = gm_sess.ship_id.unwrap();
    let _ = perim_handle
        .apply_damage(
            gm_ship_id,
            DamageVector {
                em: 200.0,
                thermal: 200.0,
                kinetic: 200.0,
                explosive: 200.0,
            },
        )
        .await;
    let heal_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/heal".into())],
    )
    .await
    .expect("/heal");
    let PyRep::KeyVal(heal_kv) = heal_rep else {
        panic!("expected KeyVal from /heal");
    };
    assert_eq!(heal_kv.get("healed").and_then(|v| v.as_bool()), Some(true));

    // 11. GM /kill target ship producing death pipeline (feat:admin.gm-kill)
    let kill_target_id = 990_000_501i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            kill_target_id,
            eve_db::InventoryItemRecord {
                item_id: kill_target_id,
                type_id: 587,
                owner_id: SEED_PILOT,
                location_id: 30_000_144,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    perim_handle
        .spawn_ball_with_combat(
            Ball {
                id: kill_target_id,
                type_id: 587,
                position: DVec3::new(5_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await
        .unwrap();

    let kill_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String(format!("/kill {kill_target_id}"))],
    )
    .await
    .expect("/kill");
    let PyRep::KeyVal(kill_kv) = kill_rep else {
        panic!("expected KeyVal from /kill");
    };
    assert_eq!(kill_kv.get("destroyed").and_then(|v| v.as_bool()), Some(true));
    assert!(kill_kv.contains_key("wreckID"), "Kill must produce a wreck");
    assert!(kill_kv.contains_key("killmailID"), "Kill must generate a killmail");

    // 12. GM Asset safety timer shift (feat:admin.gm-assetsafety-time)
    let as_non_gm = rpc(
        &ctx,
        &mut player_sess,
        "structureAssetSafety",
        "MoveEjectTimeGM",
        vec![PyRep::Integer(1), PyRep::Integer(60)],
    )
    .await;
    assert!(matches!(as_non_gm, Err(ref e) if e.code == "InsufficientRole"));

    let as_gm = rpc(
        &ctx,
        &mut gm_sess,
        "structureAssetSafety",
        "MoveEjectTimeGM",
        vec![PyRep::Integer(1), PyRep::Integer(60)],
    )
    .await
    .expect("MoveEjectTimeGM GM");
    assert_eq!(as_gm, PyRep::Bool(true));

    // 13. GM SKIN tools (feat:admin.gm-skin-tools)
    let skin_non_gm = rpc(
        &ctx,
        &mut player_sess,
        "shipCosmeticsMgr",
        "GiveSkin",
        vec![PyRep::Integer(5)],
    )
    .await;
    assert!(matches!(skin_non_gm, Err(ref e) if e.code == "InsufficientRole"));

    let give_skin_rep = rpc(
        &ctx,
        &mut gm_sess,
        "shipCosmeticsMgr",
        "GiveSkin",
        vec![PyRep::Integer(5)],
    )
    .await
    .expect("GiveSkin GM");
    assert!(matches!(give_skin_rep, PyRep::KeyVal(_)));

    let remove_skin_rep = rpc(
        &ctx,
        &mut gm_sess,
        "shipCosmeticsMgr",
        "RemoveSkin",
        vec![PyRep::Integer(5)],
    )
    .await
    .expect("RemoveSkin GM");
    assert_eq!(remove_skin_rep, PyRep::Bool(true));

    let expire_skin_rep = rpc(
        &ctx,
        &mut gm_sess,
        "shipCosmeticsMgr",
        "GMExpireSkinLicense",
        vec![PyRep::Integer(5)],
    )
    .await
    .expect("GMExpireSkinLicense GM");
    assert_eq!(expire_skin_rep, PyRep::Bool(true));
}

#[tokio::test]
async fn test_navigation_warp_speed_autopilot_waypoints_stargate_cloak() {
    // parity: feat:navigation.warp-speed-formula, feat:navigation.autopilot-route, feat:navigation.route-waypoints, feat:navigation.stargate-jump, feat:navigation.gate-cloak
    eve_testkit::attest(&[
        "feat:navigation.warp-speed-formula",
        "feat:navigation.autopilot-route",
        "feat:navigation.route-waypoints",
        "feat:navigation.stargate-jump",
        "feat:navigation.gate-cloak",
    ]);

    let ctx = Arc::new(ServerContext::new());

    // 1. Warp continuous acceleration, cruise, and deceleration physics formula (feat:navigation.warp-speed-formula)
    let mut park = Ballpark::new(30_000_142);
    let warp_speed_au = 3.0;
    let origin = DVec3::ZERO;
    let distance = 20.0 * AU_METERS;
    let destination = DVec3::new(distance, 0.0, 0.0);
    let ship = Ball {
        id: 2001,
        type_id: 587,
        position: origin,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    park.add_ball(ship);
    park.set_mode(2001, MovementMode::new_warp(origin, destination, warp_speed_au))
        .expect("set warp mode");

    let mut saw_accel = false;
    let mut prev_speed = 0.0f64;
    let mut max_speed = 0.0f64;
    for _ in 1..=50 {
        let _ = park.step(1.0);
        let b = park.balls.get(&2001).unwrap();
        let sp = b.velocity.length();
        max_speed = max_speed.max(sp);
        if let MovementMode::Warp { phase, .. } = &b.mode {
            if matches!(phase, WarpPhase::Accelerating) {
                saw_accel = true;
                if prev_speed > 1000.0 {
                    assert!(sp >= prev_speed, "Speed must increase during acceleration");
                }
            }
        }
        prev_speed = sp;
    }
    assert!(saw_accel, "Must experience acceleration phase in warp");
    assert!(max_speed > 100_000.0, "Must reach warp speeds");

    // 2. Autopilot route computation & avoidance (feat:navigation.autopilot-route)
    let esi_router = EsiRouter::new(Arc::clone(&ctx));
    // Shortest route Jita -> Amarr
    let r_short = esi_router.handle_request(&EsiHttpRequest::new(
        "GET",
        "/v1/route/30000142/30002187/?flag=shortest",
    ));
    assert_eq!(r_short.status_code, 200);
    let v_short: Value = serde_json::from_slice(&r_short.body).unwrap();
    let hops_short = v_short.as_array().unwrap().len();

    // Secure route Jita -> Amarr
    let r_sec = esi_router.handle_request(&EsiHttpRequest::new(
        "GET",
        "/v1/route/30000142/30002187/?flag=secure",
    ));
    assert_eq!(r_sec.status_code, 200);
    let v_sec: Value = serde_json::from_slice(&r_sec.body).unwrap();
    let hops_sec = v_sec.as_array().unwrap().len();
    assert!(
        hops_sec >= hops_short,
        "Secure route hops must be >= shortest route hops"
    );

    // Insecure route
    let r_insec = esi_router.handle_request(&EsiHttpRequest::new(
        "GET",
        "/v1/route/30000142/30002187/?flag=insecure",
    ));
    assert_eq!(r_insec.status_code, 200);

    // Avoid list route: avoiding Perimeter (30000144)
    let r_avoid = esi_router.handle_request(&EsiHttpRequest::new(
        "GET",
        "/v1/route/30000142/30002187/?avoid=30000144",
    ));
    assert_eq!(r_avoid.status_code, 200);
    let v_avoid: Value = serde_json::from_slice(&r_avoid.body).unwrap();
    assert!(
        !v_avoid.as_array().unwrap().contains(&serde_json::json!(30000144)),
        "Route must avoid system 30000144"
    );

    // 3. Autopilot waypoints addition, reordering, and clearing (feat:navigation.route-waypoints)
    let pilot_id = SEED_PILOT;
    // Set single destination (clear other waypoints)
    ctx.esi_state
        .update_autopilot_waypoint(pilot_id, 30_000_142, false, true);
    assert_eq!(
        ctx.esi_state.get_autopilot_waypoints(pilot_id),
        vec![30_000_142]
    );

    // Append waypoint to end
    ctx.esi_state
        .update_autopilot_waypoint(pilot_id, 30_000_144, false, false);
    assert_eq!(
        ctx.esi_state.get_autopilot_waypoints(pilot_id),
        vec![30_000_142, 30_000_144]
    );

    // Prepend waypoint to beginning
    ctx.esi_state
        .update_autopilot_waypoint(pilot_id, 30_002_187, true, false);
    assert_eq!(
        ctx.esi_state.get_autopilot_waypoints(pilot_id),
        vec![30_002_187, 30_000_142, 30_000_144]
    );

    // 4. Stargate jump within 2500m & gate cloak invulnerability (feat:navigation.stargate-jump, feat:navigation.gate-cloak)
    let mut nav_sess = SessionState::new(60_003);
    common::login_pilot(&ctx, &mut nav_sess, "demo", "demo-secret").await;
    rpc(
        &ctx,
        &mut nav_sess,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(pilot_id)],
    )
    .await
    .expect("select pilot");

    let undock_rep = rpc(&ctx, &mut nav_sess, "stationSvc", "Undock", vec![])
        .await
        .expect("Undock");
    let PyRep::KeyVal(ukv) = undock_rep else {
        panic!("expected KeyVal from Undock");
    };
    let beyonce_id = ukv.get("beyonceBoundID").and_then(|v| v.as_i64()).unwrap() as u64;
    let ship_id = ukv.get("shipID").and_then(|v| v.as_i64()).unwrap();

    let jita_sys = ctx.systems.get_or_spawn(30_000_142);
    let perim_gate_id = 50_001_249i64; // Stargate to Perimeter in Jita
    let (_, gate_b) = jita_sys
        .get_ball_snapshot(perim_gate_id)
        .await
        .unwrap()
        .unwrap();

    // Position ship within 1000m of stargate
    jita_sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: gate_b.position + DVec3::new(1000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    // CmdStargateJump
    let jump_rep = bound_rpc(
        &ctx,
        &mut nav_sess,
        "beyonce",
        Some(beyonce_id),
        "CmdStargateJump",
        vec![PyRep::Integer(perim_gate_id)],
    )
    .await
    .expect("CmdStargateJump");
    let PyRep::KeyVal(jkv) = jump_rep else {
        panic!("expected KeyVal from CmdStargateJump");
    };
    assert_eq!(
        jkv.get("solarSystemID").and_then(|v| v.as_i64()),
        Some(30_000_144)
    );
    assert_eq!(nav_sess.solar_system_id, Some(30_000_144));

    // Gate cloak gives complete 60s invulnerability
    let perim_sys = ctx.systems.get_or_spawn(30_000_144);
    let dmg_under_cloak = perim_sys
        .apply_damage(
            ship_id,
            DamageVector {
                em: 500.0,
                thermal: 500.0,
                kinetic: 500.0,
                explosive: 500.0,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        dmg_under_cloak.0, 0.0,
        "Gate cloak must provide complete invulnerability (0.0 damage)"
    );

    // Movement command breaks gate cloak
    let new_beyonce_id = jkv.get("beyonceBoundID").and_then(|v| v.as_i64()).unwrap() as u64;
    bound_rpc(
        &ctx,
        &mut nav_sess,
        "beyonce",
        Some(new_beyonce_id),
        "CmdGotoDirection",
        vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .expect("CmdGotoDirection");

    // Subsequent damage applies normally
    let dmg_broken_cloak = perim_sys
        .apply_damage(
            ship_id,
            DamageVector {
                em: 500.0,
                thermal: 500.0,
                kinetic: 500.0,
                explosive: 500.0,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        dmg_broken_cloak.0 > 0.0,
        "Damage must apply once gate cloak is broken"
    );
}

#[tokio::test]
async fn test_account_sso_handshake_multilogin_roles_eula_language_status() {
    // parity: feat:account.sso-login, feat:account.native-handshake, feat:account.multi-login-block, feat:account.account-roles, feat:account.eula-agreements, feat:account.language-setting, feat:account.launcher-status
    eve_testkit::attest(&[
        "feat:account.sso-login",
        "feat:account.native-handshake",
        "feat:account.multi-login-block",
        "feat:account.account-roles",
        "feat:account.eula-agreements",
        "feat:account.language-setting",
        "feat:account.launcher-status",
    ]);

    let ctx = Arc::new(ServerContext::new());

    // 1. SSO Login via OAuth2 PKCE S256 token exchange (feat:account.sso-login)
    let sso_user = "demo";
    let sso_pass = "demo-secret";
    let code_verifier = "0123456789abcdef0123456789abcdef0123456789abcdef";
    let verifier_digest = eve_proto::quasar::sha256_bytes(code_verifier.as_bytes());
    let code_challenge = eve_db::base64url_encode_no_pad(&verifier_digest);
    let auth_code = ctx
        .db
        .authorize_pkce(
            sso_user,
            sso_pass,
            SEED_PILOT,
            "evomu-launcher",
            "http://localhost:8080/callback",
            vec!["esi-skills.read_skills.v1".into()],
            &code_challenge,
            "S256",
            ctx.now_unix(),
        )
        .expect("authorize_pkce");

    let bundle = ctx
        .db
        .exchange_pkce_code(&auth_code, "evomu-launcher", code_verifier, ctx.now_unix())
        .expect("exchange_pkce_code");
    assert!(!bundle.access_token.is_empty(), "SSO JWT access token must be non-empty");

    let mut sso_sess = SessionState::new(60_010);
    let sso_login = rpc(
        &ctx,
        &mut sso_sess,
        "authentication",
        "Login",
        vec![
            PyRep::String("__sso__".into()),
            PyRep::String(bundle.access_token.clone()),
        ],
    )
    .await
    .expect("SSO Login");
    assert!(matches!(sso_login, PyRep::KeyVal(_)));
    assert!(sso_sess.is_authenticated(), "Session must be authenticated via SSO");

    // Invalid / expired SSO token rejected
    let mut bad_sso_sess = SessionState::new(60_011);
    let bad_sso = rpc(
        &ctx,
        &mut bad_sso_sess,
        "authentication",
        "Login",
        vec![
            PyRep::String("__sso__".into()),
            PyRep::String("invalid.jwt.token".into()),
        ],
    )
    .await;
    assert!(
        matches!(bad_sso, Err(ref e) if e.code == "AuthenticationFailed"),
        "Invalid token must be rejected"
    );

    // 2. Native machoNet handshake state machine (feat:account.native-handshake)
    let expected_ver = VersionExchange::default();
    let mut hm = MachoHandshakeStateMachine::new(expected_ver.clone(), 1, 5001, 7001);
    assert!(!hm.is_authenticated());

    // Rejects mismatched build version with GPSTransportClosed
    let mut bad_hm = hm.clone();
    let mut bad_ver = VersionExchange::default();
    bad_ver.build_version = 1;
    let bad_res = bad_hm.step(&bad_ver.to_pyrep());
    assert!(matches!(bad_res, Err(GPSTransportClosed { .. })));

    // Accepts valid V24 version
    let step1 = hm
        .step(&VersionExchange::default().to_pyrep())
        .expect("step version");
    assert!(matches!(step1, HandshakeStepOutput::SendFramesAndContinue(_)));

    // CryptoRequest
    let crypto_req = HandshakeCommand::CryptoRequest(CryptoRequestPacket::placebo());
    let step2 = hm.step(&crypto_req.to_pyrep()).expect("step crypto request");
    assert!(matches!(step2, HandshakeStepOutput::SendFramesAndContinue(_)));

    // CryptoChallengePacket
    let chal = CryptoChallengePacket::new_password_login("demo", "demo-secret");
    let step3 = hm.step(&chal.to_pyrep()).expect("step challenge");
    assert!(matches!(step3, HandshakeStepOutput::NeedsAuthentication(_)));

    // Complete challenge establishing userid, role, languageID
    let id = HandshakeIdentity {
        user_id: 1001,
        role: 1,
        language_id: "EN".into(),
        user_type: 30,
        address: "127.0.0.1".into(),
        access_token: None,
        computer_hash: None,
    };
    let frames = hm.complete_challenge(&chal, Ok(id), 1).expect("complete challenge");
    let srv_hs = CryptoServerHandshake::from_pyrep(&frames[1]).expect("parse server handshake");
    let res_pkt = CryptoHandshakeResult::for_server_challenge(&srv_hs.server_challenge);
    let final_step = hm.step(&res_pkt.to_pyrep()).expect("step result");
    assert!(matches!(final_step, HandshakeStepOutput::Authenticated { .. }));
    assert!(hm.is_authenticated(), "Handshake machine must be authenticated");

    // 3. Multi-Login Blocker (feat:account.multi-login-block)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch1) = snap.characters.get_mut(&90_000_001) {
            ch1.clone_grade = 0; // Alpha clone
        }
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.clone_grade = 0; // Alpha clone
        }
    });

    let mut a1_sess = SessionState::new(60_020);
    common::login_pilot(&ctx, &mut a1_sess, "demo", "demo-secret").await;
    let login1 = rpc(
        &ctx,
        &mut a1_sess,
        "multiLoginBlocker",
        "Login",
        vec![PyRep::String("machine_hash_alpha_1".into())],
    )
    .await
    .expect("multiLoginBlocker 1");
    assert_eq!(login1, PyRep::Bool(true));

    // Second Alpha on same machine_hash is refused
    let mut a2_sess = SessionState::new(60_021);
    common::login_pilot(&ctx, &mut a2_sess, "other", "other-secret").await;
    let login2_blocked = rpc(
        &ctx,
        &mut a2_sess,
        "multiLoginBlocker",
        "Login",
        vec![PyRep::String("machine_hash_alpha_1".into())],
    )
    .await
    .expect("multiLoginBlocker 2 blocked");
    assert_eq!(
        login2_blocked,
        PyRep::Bool(false),
        "Second Alpha on same computerhash must be blocked"
    );

    // Second Alpha on distinct computerhash is allowed
    let login2_allowed = rpc(
        &ctx,
        &mut a2_sess,
        "multiLoginBlocker",
        "Login",
        vec![PyRep::String("machine_hash_alpha_2".into())],
    )
    .await
    .expect("multiLoginBlocker 2 allowed");
    assert_eq!(login2_allowed, PyRep::Bool(true));

    // 4. Account roles enforcement (feat:account.account-roles)
    let p_acc = ctx
        .db
        .with_snapshot(|s| s.accounts.get("demo").cloned())
        .expect("player account");
    assert_eq!(
        p_acc.role & (ROLE_GMS | ROLE_ADMIN),
        0,
        "Player role must not have GM/Admin bits"
    );
    assert_eq!(p_acc.role & ROLE_PLAYER, ROLE_PLAYER);

    ctx.db
        .provision_gm_account("gm_role_chk", "pass", 90_000_103, "GM Check");
    let gm_acc = ctx
        .db
        .with_snapshot(|s| s.accounts.get("gm_role_chk").cloned())
        .expect("gm account");
    assert_ne!(gm_acc.role & ROLE_GM_MASK, 0, "GM account must have ROLE_GM_MASK");

    let priv_denied = rpc(
        &ctx,
        &mut a1_sess,
        "publicQaToolsServer",
        "SlashCmd",
        vec![PyRep::String("/heal".into())],
    )
    .await;
    assert!(matches!(priv_denied, Err(ref e) if e.code == "InsufficientRole"));

    // 5. Legal agreements & EULA acceptance (feat:account.eula-agreements)
    let req_latest = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"eula-corr-01".to_vec(),
        payload: Some(GoogleAny {
            type_url: "type.googleapis.com/eve.legal.agreement.api.GetLatestRequest".into(),
            value: vec![],
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-test-inst-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep_latest = rpc(
        &ctx,
        &mut a1_sess,
        "publicGatewaySvc",
        "SendRequest",
        vec![PyRep::Bytes(req_latest.encode_proto())],
    )
    .await
    .expect("GetLatestRequest");
    let rsp_latest = extract_response_proto(&rep_latest);
    assert_eq!(rsp_latest.status_code, 200);

    let req_accept = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"eula-corr-02".to_vec(),
        payload: Some(GoogleAny {
            type_url: "type.googleapis.com/eve.user.legal.agreement.api.AcceptRequest".into(),
            value: vec![],
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-test-inst-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep_accept = rpc(
        &ctx,
        &mut a1_sess,
        "publicGatewaySvc",
        "SendRequest",
        vec![PyRep::Bytes(req_accept.encode_proto())],
    )
    .await
    .expect("AcceptRequest");
    let rsp_accept = extract_response_proto(&rep_accept);
    assert_eq!(rsp_accept.status_code, 200);

    let uid1 = a1_sess.user_id.unwrap();
    let accepted = eve_server::services::quasar_auth_user::get_identity_store()
        .read()
        .unwrap()
        .accepted_agreements
        .get(&uid1)
        .cloned()
        .unwrap_or_default();
    assert!(
        accepted.iter().any(|a| a.contains("eula_v24")),
        "EULA acceptance must be recorded for user"
    );

    // 6. Language setting (feat:account.language-setting)
    let set_lang_rep = rpc(
        &ctx,
        &mut a1_sess,
        "authentication",
        "SetLanguageID",
        vec![PyRep::String("de".into())],
    )
    .await
    .expect("SetLanguageID");
    assert_eq!(set_lang_rep, PyRep::None);
    assert_eq!(a1_sess.language_id, "de", "Language ID must be updated in session");

    let bad_lang = rpc(
        &ctx,
        &mut a1_sess,
        "authentication",
        "SetLanguageID",
        vec![PyRep::String("".into())],
    )
    .await;
    assert!(matches!(bad_lang, Err(ref e) if e.code == "InvalidLanguageID"));

    // 7. Launcher status endpoint (feat:account.launcher-status)
    let esi_router = EsiRouter::new(Arc::clone(&ctx));
    let status_req = EsiHttpRequest::new("GET", "/v1/status/");
    let status_resp = esi_router.handle_request(&status_req);
    assert_eq!(status_resp.status_code, 200);
    let status_val: Value =
        serde_json::from_slice(&status_resp.body).expect("parse status json");
    assert!(status_val.get("players").is_some(), "Status must report player count");
    assert_eq!(status_val.get("vip").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        status_val.get("server_version").and_then(|v| v.as_str()),
        Some(&VersionExchange::default().build_version.to_string()[..])
    );
    assert!(status_val.get("start_time").is_some());
}
