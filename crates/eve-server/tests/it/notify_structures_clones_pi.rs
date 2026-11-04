//! Real server-side emission tests for Notify Batch 7 (Part 1: Mining, Probes, Scanners & Space).

use crate::common;
use eve_db::{AsteroidRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode, SlimItemMeta, TargetLock};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use std::collections::HashMap;

pub(crate) const PILOT_A: i64 = 90_000_001;
pub(crate) const PILOT_B: i64 = 90_000_002;
pub(crate) const PILOT_C: i64 = 90_000_003;
pub(crate) const SYS_JITA: u32 = 30_000_142;
pub(crate) const SYS_POCHVEN_KUHARAH: u32 = 30_000_021;
pub(crate) const MOON_JITA: i64 = 40_009_087;
pub(crate) const MOON_POCHVEN: i64 = 40_001_416;
pub(crate) const JITA_STATION: i64 = 60_003_760;

pub(crate) fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx.set_mechanic_rng_seed(Some(42));
    ctx
}

pub(crate) async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

pub(crate) async fn lock_target(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, ship_id: i64, target_id: i64) {
    rpc(ctx, sess, "dogmaIM", "AddTarget", &[PyRep::Integer(target_id)]).await.expect("AddTarget");
    let lock = TargetLock { target_id, lock_time_ms: 100.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None };
    let _ = ctx.systems.get_or_spawn(sys_id).add_target_lock(ship_id, lock).await;
}

pub(crate) fn make_item(
    item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
}

pub(crate) async fn login_in_space(
    ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64, ship_id: i64, ship_type: u32, sys_id: u32, pos: DVec3,
) -> SessionState {
    if char_id == PILOT_C {
        ctx.db.with_mut_snapshot(|snap| {
            snap.accounts.insert("third".to_string(), eve_db::AccountRecord {
                user_id: 3, username: "third".to_string(),
                password_token: eve_db::WorldRepository::hash_password_token("third", "third-secret"),
                role: eve_machonet::service_roles::ROLE_PLAYER_DEFAULT, banned_until_unix: None, ban_reason: None,
            });
            let mut template = snap.characters[&PILOT_A].clone();
            template.character_id = PILOT_C;
            template.user_id = 3;
            template.name = "Third Capsuleer".to_string();
            snap.characters.insert(PILOT_C, template);
        });
    }
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    rpc(ctx, &mut s, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
    ctx.db.with_mut_snapshot(|snap| {
        let template = snap.characters[&PILOT_A].clone();
        let ch = snap.characters.entry(char_id).or_insert_with(|| template);
        ch.character_id = char_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.items.insert(ship_id, make_item(ship_id, ship_type, char_id, sys_id as i64, InventoryFlag::None, 1, true));
    });
    s.solar_system_id = Some(sys_id);
    s.station_id = None;
    s.structure_id = None;
    s.ship_id = Some(ship_id);

    let handle = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type);
    cs.is_cloaked = false;
    cs.gate_cloak_remaining_ticks = 0;
    cs.undock_invulnerable_remaining_ticks = 0;
    let slim = ctx.build_ship_slim_meta(char_id, ship_type);
    let ball = Ball {
        id: ship_id, type_id: ship_type, position: pos, velocity: DVec3::ZERO,
        max_velocity: 300.0, inertia: 1.0, mass: 1_200_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let _ = handle.spawn_ball_with_meta(ball, cs, slim).await;
    ctx.advance_secs(12);
    let _ = drain_all(ctx, &mut s, char_id);
    s
}

pub(crate) fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out: Vec<_> = ctx.drain_notifications_for_char(char_id).into_iter().map(|n| (n.event, n.args)).collect();
    out.extend(sess.drain_notifications());
    out
}

pub(crate) fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(e, _)| e == event)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| panic!("expected notification {event} in {notifs:?}"))
}

// parity: notify:OnOreMined, notify:OnAsteroidTerminated, notify:OnNewMoonProbe
// parity: notify:OnMoonScanComplete, notify:OnRemoveMoonProbe, notify:OnScanNoMaterials
// parity: notify:OnRemoveProbe, notify:OnCargoScanComplete, notify:OnShipScanCompleted
// parity: notify:OnDamageStateChange, notify:OnMachoObjectDisconnect
// parity: notify:OnWormholeJumpCancel, notify:OnRemoteMessage
#[tokio::test]
async fn notify_batch7_mining_probes_scanners_and_space() {
    eve_testkit::attest(&[
        "notify:OnOreMined",
        "notify:OnAsteroidTerminated",
        "notify:OnNewMoonProbe",
        "notify:OnMoonScanComplete",
        "notify:OnRemoveMoonProbe",
        "notify:OnScanNoMaterials",
        "notify:OnRemoveProbe",
        "notify:OnCargoScanComplete",
        "notify:OnShipScanCompleted",
        "notify:OnDamageStateChange",
        "notify:OnMachoObjectDisconnect",
        "notify:OnWormholeJumpCancel",
        "notify:OnRemoteMessage",
    ]);

    let ctx = new_ctx();
    let (ship_a, ship_b, pod_c) = (510_000_001_i64, 510_000_002_i64, 510_000_003_i64);
    let mut sess_a = login_in_space(&ctx, 7_101, "demo", "demo-secret", PILOT_A, ship_a, 32880, SYS_JITA, DVec3::ZERO).await;
    let mut sess_b = login_in_space(&ctx, 7_102, "other", "other-secret", PILOT_B, ship_b, 587, SYS_JITA, DVec3::new(2_000.0, 0.0, 0.0)).await;
    let mut sess_c = login_in_space(&ctx, 7_103, "third", "third-secret", PILOT_C, pod_c, 670, SYS_JITA, DVec3::new(2_500.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(PILOT_A).or_default();
        for (&tid, ty) in &ctx.sde.types {
            if ty.category_id == 16 {
                sk.insert(tid, eve_db::TrainedSkillRecord { type_id: tid, level: 5, skill_points: 256_000.0 });
            }
        }
    });
    ctx.invalidate_ship_fitting(ship_a);

    // 1-2. OnOreMined (1-arg V24 dataDict) + OnAsteroidTerminated (1-arg asteroidID)
    let (miner_id, ast_id) = (710_000_101_i64, 710_000_102_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(miner_id, make_item(miner_id, 483, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(miner_id, true);
        snap.items.insert(
            ast_id,
            make_item(ast_id, 1230, 0, SYS_JITA as i64, InventoryFlag::None, 100, true),
        );
        snap.asteroids.insert(
            ast_id,
            AsteroidRecord {
                asteroid_id: ast_id,
                solar_system_id: SYS_JITA,
                belt_id: 0,
                type_id: 1230,
                quantity: 100,
                initial_quantity: 100,
                position: (1_500.0, 0.0, 0.0),
                radius: 100.0,
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_a);
    let jita = ctx.systems.get_or_spawn(SYS_JITA);
    let _ = jita
        .spawn_ball(Ball {
            id: ast_id,
            type_id: 1230,
            position: DVec3::new(1_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    lock_target(&ctx, &mut sess_a, SYS_JITA, ship_a, ast_id).await;
    rpc(&ctx, &mut sess_a, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(ship_a), PyRep::Integer(miner_id)])
        .await
        .expect("online miner");
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(miner_id), PyRep::String("miningLaser".into()), PyRep::Integer(ast_id)])
        .await
        .expect("activate miner");
    ctx.advance_secs(65);
    tick_background_cluster(&ctx).await;

    let mine_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let ore_args = find_notif(&mine_notifs, "OnOreMined");
    assert_eq!(ore_args.len(), 1);
    assert_eq!(ore_args[0].dict_get("oreType").and_then(|v| v.as_i64()), Some(1230));
    assert_eq!(ore_args[0].dict_get("quantity_added").and_then(|v| v.as_i64()), Some(100));
    assert_eq!(ore_args[0].dict_get("quantity_removed").and_then(|v| v.as_i64()), Some(100));
    assert_eq!(ore_args[0].dict_get("hasRewards").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(ore_args[0].dict_get("itemID").and_then(|v| v.as_i64()), Some(ast_id));
    assert_eq!(ore_args[0].dict_get("moduleItemID").and_then(|v| v.as_i64()), Some(miner_id));
    assert_eq!(find_notif(&mine_notifs, "OnAsteroidTerminated"), &[PyRep::Integer(ast_id)]);

    // 3-5. OnNewMoonProbe (1-arg keyval), OnMoonScanComplete (2-arg), OnRemoveMoonProbe (1-arg)
    let (moon_launcher_id, gaze_probe_id) = (710_000_111_i64, 710_000_112_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, it| !(it.location_id == ship_a && it.item_id != ship_a));
        snap.items.insert(moon_launcher_id, make_item(moon_launcher_id, 33272, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(moon_launcher_id, true);
        snap.items.insert(gaze_probe_id, make_item(gaze_probe_id, 18637, PILOT_A, ship_a, InventoryFlag::HiSlot0, 2, false));
    });
    ctx.invalidate_ship_fitting(ship_a);
    let moon_jita_pos = jita.get_ball_snapshot(MOON_JITA).await.unwrap().unwrap().1.position;
    let ship_near_jita_moon = moon_jita_pos - DVec3::new(10_000_000.0, 0.0, 0.0);
    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_a).await {
        sb.position = ship_near_jita_moon;
        sb.velocity = (moon_jita_pos - ship_near_jita_moon).normalize() * 100.0;
        let _ = jita.spawn_ball(sb).await;
    }
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(&ctx, &mut sess_a, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(ship_a), PyRep::Integer(moon_launcher_id)])
        .await
        .expect("online moon launcher");
    let moon_launch = rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(moon_launcher_id), PyRep::String("useMissiles".into()), PyRep::Integer(MOON_JITA)])
        .await
        .expect("launch moon probe");
    let probe_id = moon_launch.dict_get("probeID").and_then(|v| v.as_i64()).expect("probeID");
    let new_probe_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let new_probe_args = find_notif(&new_probe_notifs, "OnNewMoonProbe");
    assert_eq!(new_probe_args.len(), 1);
    assert_eq!(new_probe_args[0].dict_get("probeID").and_then(|v| v.as_i64()), Some(probe_id));
    assert_eq!(new_probe_args[0].dict_get("typeID").and_then(|v| v.as_i64()), Some(18637));
    assert_eq!(new_probe_args[0].dict_get("moonID").and_then(|v| v.as_i64()), Some(MOON_JITA));
    assert!(new_probe_args[0].dict_get("expiry").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    ctx.advance_secs(140);
    let scan_done_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let moon_scan_args = find_notif(&scan_done_notifs, "OnMoonScanComplete");
    assert_eq!(moon_scan_args.len(), 2);
    assert_eq!(moon_scan_args[0], PyRep::Integer(MOON_JITA));
    let PyRep::Dict(moon_ores) = &moon_scan_args[1] else { panic!("expected Dict for moon scan") };
    assert!(!moon_ores.is_empty());
    for (k, v) in moon_ores {
        let tid = k.as_i64().expect("ore typeID") as u32;
        let frac = v.as_f64().expect("ore fraction");
        assert!(ctx.sde.types.contains_key(&tid) && (0.0..=1.0).contains(&frac));
    }
    assert_eq!(find_notif(&scan_done_notifs, "OnRemoveMoonProbe"), &[PyRep::Integer(probe_id)]);

    // 6. OnScanNoMaterials (1-arg moonID) on Pochven moon (40001416 in Kuharah 30000021)
    let pochven = ctx.systems.get_or_spawn(SYS_POCHVEN_KUHARAH);
    let moon_poch_pos = pochven.get_ball_snapshot(MOON_POCHVEN).await.unwrap().unwrap().1.position;
    let ship_near_poch_moon = moon_poch_pos - DVec3::new(10_000_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = SYS_POCHVEN_KUHARAH;
        }
        if let Some(sh) = snap.items.get_mut(&ship_a) {
            sh.location_id = SYS_POCHVEN_KUHARAH as i64;
        }
    });
    sess_a.solar_system_id = Some(SYS_POCHVEN_KUHARAH);
    let mut poch_cs = ctx.build_ship_combat_state(PILOT_A, ship_a, 32880);
    poch_cs.is_cloaked = false;
    poch_cs.gate_cloak_remaining_ticks = 0;
    poch_cs.undock_invulnerable_remaining_ticks = 0;
    let _ = pochven
        .spawn_ball_with_meta(
            Ball {
                id: ship_a, type_id: 32880, position: ship_near_poch_moon,
                velocity: (moon_poch_pos - ship_near_poch_moon).normalize() * 100.0,
                max_velocity: 300.0, inertia: 1.0, mass: 1_200_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            poch_cs,
            ctx.build_ship_slim_meta(PILOT_A, 32880),
        )
        .await;
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(moon_launcher_id), PyRep::String("useMissiles".into()), PyRep::Integer(MOON_POCHVEN)])
        .await
        .expect("launch Pochven moon probe");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    ctx.advance_secs(140);
    let poch_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&poch_notifs, "OnScanNoMaterials"), &[PyRep::Integer(MOON_POCHVEN)]);

    // Move Pilot A back to Jita at DVec3::ZERO for scanner, probe, weapon & wormhole checks
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = SYS_JITA;
        }
        if let Some(sh) = snap.items.get_mut(&ship_a) {
            sh.location_id = SYS_JITA as i64;
            sh.type_id = 587; // Rifter
        }
    });
    ctx.invalidate_ship_fitting(ship_a);
    sess_a.solar_system_id = Some(SYS_JITA);
    let mut jita_cs = ctx.build_ship_combat_state(PILOT_A, ship_a, 587);
    jita_cs.is_cloaked = false;
    jita_cs.gate_cloak_remaining_ticks = 0;
    jita_cs.undock_invulnerable_remaining_ticks = 0;
    let _ = jita
        .spawn_ball_with_meta(
            Ball {
                id: ship_a, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO,
                max_velocity: 350.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            jita_cs,
            ctx.build_ship_slim_meta(PILOT_A, 587),
        )
        .await;

    // 7. OnRemoveProbe (1-arg probeID)
    let (core_launcher_id, core_probe_id) = (710_000_121_i64, 710_000_122_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, it| !(it.location_id == ship_a && it.item_id != ship_a));
        snap.items.insert(core_launcher_id, make_item(core_launcher_id, 17938, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(core_launcher_id, true);
        snap.items.insert(core_probe_id, make_item(core_probe_id, 30013, PILOT_A, ship_a, InventoryFlag::HiSlot0, 8, false));
    });
    ctx.invalidate_ship_fitting(ship_a);
    rpc(&ctx, &mut sess_a, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(ship_a), PyRep::Integer(core_launcher_id)])
        .await
        .expect("online core launcher");
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(core_launcher_id), PyRep::String("launchProbe".into())])
        .await
        .expect("launch core probes");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let destroy_rep = rpc(&ctx, &mut sess_a, "scanMgr", "DestroyProbe", &[]).await.expect("DestroyProbe");
    let destroyed_pid = destroy_rep.dict_get("destroyedProbeID").and_then(|v| v.as_i64()).expect("destroyedProbeID");
    let rm_probe_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&rm_probe_notifs, "OnRemoveProbe"), &[PyRep::Integer(destroyed_pid)]);

    // 8-9. OnCargoScanComplete (2-arg) & OnShipScanCompleted (4-arg)
    let (cargo_scan_id, ship_scan_id, trit_id, b_gun_id) = (710_000_131_i64, 710_000_132_i64, 710_000_133_i64, 710_000_134_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(cargo_scan_id, make_item(cargo_scan_id, 442, PILOT_A, ship_a, InventoryFlag::MedSlot0, 1, true));
        snap.items.insert(ship_scan_id, make_item(ship_scan_id, 443, PILOT_A, ship_a, InventoryFlag::MedSlot1, 1, true));
        snap.items.insert(trit_id, make_item(trit_id, 34, PILOT_B, ship_b, InventoryFlag::Cargo, 500, false));
        snap.items.insert(b_gun_id, make_item(b_gun_id, 2873, PILOT_B, ship_b, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(cargo_scan_id, true);
        snap.module_online_states.insert(ship_scan_id, true);
        snap.module_online_states.insert(b_gun_id, true);
    });
    ctx.invalidate_ship_fitting(ship_a);
    ctx.invalidate_ship_fitting(ship_b);
    rpc(&ctx, &mut sess_a, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(ship_a), PyRep::Integer(cargo_scan_id)])
        .await
        .expect("online cargo scanner");
    rpc(&ctx, &mut sess_a, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(ship_a), PyRep::Integer(ship_scan_id)])
        .await
        .expect("online ship scanner");
    lock_target(&ctx, &mut sess_a, SYS_JITA, ship_a, ship_b).await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(cargo_scan_id), PyRep::String("online".into()), PyRep::Integer(ship_b)])
        .await
        .expect("cargo scan");
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(ship_scan_id), PyRep::String("online".into()), PyRep::Integer(ship_b)])
        .await
        .expect("ship scan");
    let scan_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let cargo_args = find_notif(&scan_notifs, "OnCargoScanComplete");
    assert_eq!(cargo_args.len(), 2);
    assert_eq!(cargo_args[0], PyRep::Integer(ship_b));
    assert!(matches!(&cargo_args[1], PyRep::List(items) if items.contains(&PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(500)]))));
    let ship_scan_args = find_notif(&scan_notifs, "OnShipScanCompleted");
    assert_eq!(ship_scan_args.len(), 4);
    assert_eq!(ship_scan_args[0], PyRep::Integer(ship_b));
    let cap_cur = ship_scan_args[1].as_f64().expect("capacitorCharge");
    let cap_max = ship_scan_args[2].as_f64().expect("capacitorCapacity");
    assert!(cap_cur > 0.0 && cap_max >= cap_cur);
    let PyRep::List(hw) = &ship_scan_args[3] else { panic!("expected hardwareList") };
    assert!(hw.contains(&PyRep::Tuple(vec![PyRep::Integer(2873), PyRep::Integer(1)])));

    // 10-11. OnDamageStateChange (2-arg) & OnMachoObjectDisconnect (3-arg on pod death)
    let (a_gun_id, a_ammo_id) = (710_000_141_i64, 710_000_142_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(a_gun_id, make_item(a_gun_id, 2873, PILOT_A, ship_a, InventoryFlag::HiSlot1, 1, true));
        snap.items.insert(a_ammo_id, make_item(a_ammo_id, 185, PILOT_A, ship_a, InventoryFlag::HiSlot1, 100, false));
        snap.module_online_states.insert(a_gun_id, true);
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.war_faction_id = Some(500_002);
        }
        if let Some(ch) = snap.characters.get_mut(&PILOT_B) {
            ch.war_faction_id = Some(500_001);
        }
        if let Some(ch) = snap.characters.get_mut(&PILOT_C) {
            ch.war_faction_id = Some(500_001);
        }
        snap.fw_system_occupations.insert(
            SYS_JITA,
            eve_db::FwSystemOccupationRecord {
                solar_system_id: SYS_JITA,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_a);
    rpc(&ctx, &mut sess_a, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(ship_a), PyRep::Integer(a_gun_id)])
        .await
        .expect("online autocannon");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(a_gun_id), PyRep::String("projectileFired".into()), PyRep::Integer(ship_b)])
        .await
        .expect("fire at ship_b");
    assert_eq!(ctx.drain_notifications_for_char(PILOT_A).iter().filter(|n| n.event == "OnDamageStateChange").count(), 1);
    assert_eq!(sess_a.drain_notifications().iter().filter(|(e, _)| e == "OnDamageStateChange").count(), 1);
    let dmg_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(dmg_notifs_b.iter().filter(|(e, _)| e == "OnDamageStateChange").count(), 1);
    let dmg_args = find_notif(&dmg_notifs_b, "OnDamageStateChange");
    assert_eq!(dmg_args.len(), 2);
    assert_eq!(dmg_args[0], PyRep::Integer(ship_b));
    let PyRep::Tuple(ds) = &dmg_args[1] else { panic!("expected damageState tuple") };
    assert_eq!(ds.len(), 3);
    let PyRep::Tuple(sh_tup) = &ds[0] else { panic!("expected shield tuple") };
    assert_eq!(sh_tup.len(), 3);
    let (sh_r, ar_r, hu_r) = (sh_tup[0].as_f64().unwrap(), ds[1].as_f64().unwrap(), ds[2].as_f64().unwrap());
    assert!((0.0..1.0).contains(&sh_r) && ar_r == 1.0 && hu_r == 1.0);

    // 11. OnMachoObjectDisconnect (3-arg) across stargate jump, dock, undock, UnBind, and podding
    async fn macho_bind(ctx: &ServerContext, sess: &mut SessionState, svc: &str, obj_id: i64) -> (u64, PyRep, PyRep) {
        let rep = ctx
            .dispatch_rpc(
                sess, svc, None, "MachoBindObject",
                &[PyRep::Tuple(vec![PyRep::Integer(obj_id), PyRep::Integer(0)]), PyRep::None],
                &HashMap::new(), Some("machoNet:26000"),
            )
            .await
            .expect("MachoBindObject");
        let PyRep::Tuple(parts) = rep else { panic!("MachoBindObject tuple") };
        let PyRep::SubStruct(sub_s) = &parts[0] else { panic!("SubStruct OID") };
        let PyRep::SubStream(sub_t) = sub_s.as_ref() else { panic!("SubStream OID") };
        let PyRep::Tuple(pair) = sub_t.as_ref() else { panic!("(oid, refID) tuple") };
        let oid_s = pair[0].as_str().expect("oid_str");
        assert!(oid_s.starts_with("N=888444:"));
        assert!(pair[1].as_i64().unwrap_or(0) > 116_444_736_000_000_000);
        let bid: u64 = oid_s.trim_start_matches("N=888444:").parse().expect("bound_id");
        (bid, pair[0].clone(), pair[1].clone())
    }
    let macho_client_b = (1_000_000_000_000_u64 + sess_b.session_id) as i64;
    ctx.advance_secs(15);
    let (jb_bid, jb_oid, jb_ref) = macho_bind(&ctx, &mut sess_b, "beyonce", SYS_JITA as i64).await;
    let (_js_bid, js_oid, js_ref) = macho_bind(&ctx, &mut sess_b, "scanMgr", SYS_JITA as i64).await;
    let jump_edge = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == SYS_JITA && ctx.sde.stations.values().any(|st| st.solar_system_id == j.to_solar_system_id))
        .expect("Jita gate to system with station")
        .clone();
    let gate_r = ctx.sde.types.get(&16).map(|t| t.radius).unwrap_or(14_051.0);
    assert!(jita.set_ball_position(ship_b, jump_edge.position + DVec3::new(gate_r + 500.0, 0.0, 0.0)).await);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    ctx.dispatch_rpc(&mut sess_b, "beyonce", Some(jb_bid), "CmdStargateJump", &[PyRep::Integer(jump_edge.stargate_id)], &HashMap::new(), None)
        .await
        .expect("CmdStargateJump");
    let _ = sess_b.drain_notifications();
    let jump_disc: Vec<_> = ctx.drain_notifications_for_char(PILOT_B).into_iter().filter(|n| n.event == "OnMachoObjectDisconnect").map(|n| n.args).collect();
    assert_eq!(jump_disc.len(), 2);
    assert!(jump_disc.contains(&vec![jb_oid, PyRep::Integer(macho_client_b), jb_ref]));
    assert!(jump_disc.contains(&vec![js_oid, PyRep::Integer(macho_client_b), js_ref]));

    // Dock in destination system -> disconnects newly bound solar-system beyonce + scanMgr
    let dest_sys = jump_edge.to_solar_system_id;
    let dest_st = ctx.sde.stations.values().find(|st| st.solar_system_id == dest_sys).expect("dest station").clone();
    ctx.advance_secs(12);
    let (_db_bid, db_oid, db_ref) = macho_bind(&ctx, &mut sess_b, "beyonce", dest_sys as i64).await;
    let (_ds_bid, ds_oid, ds_ref) = macho_bind(&ctx, &mut sess_b, "scanMgr", dest_sys as i64).await;
    assert!(ctx.systems.get_or_spawn(dest_sys).set_ball_position(ship_b, dest_st.position + DVec3::new(1_000.0, 0.0, 0.0)).await);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(&ctx, &mut sess_b, "stationSvc", "Dock", &[PyRep::Integer(dest_st.station_id)]).await.expect("Dock");
    let _ = sess_b.drain_notifications();
    let dock_disc: Vec<_> = ctx.drain_notifications_for_char(PILOT_B).into_iter().filter(|n| n.event == "OnMachoObjectDisconnect").map(|n| n.args).collect();
    assert!(dock_disc.contains(&vec![db_oid, PyRep::Integer(macho_client_b), db_ref]));
    assert!(dock_disc.contains(&vec![ds_oid, PyRep::Integer(macho_client_b), ds_ref]));

    // Undock from station -> disconnects station + repairSvc bound objects; UnBind -> disconnects corpRegistry
    ctx.advance_secs(12);
    let (_st_bid, st_oid, st_ref) = macho_bind(&ctx, &mut sess_b, "station", dest_st.station_id).await;
    let (_rp_bid, rp_oid, rp_ref) = macho_bind(&ctx, &mut sess_b, "repairSvc", dest_st.station_id).await;
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(&ctx, &mut sess_b, "stationSvc", "Undock", &[]).await.expect("Undock");
    let _ = sess_b.drain_notifications();
    let undock_disc: Vec<_> = ctx.drain_notifications_for_char(PILOT_B).into_iter().filter(|n| n.event == "OnMachoObjectDisconnect").map(|n| n.args).collect();
    assert_eq!(undock_disc.len(), 2);
    assert!(undock_disc.contains(&vec![st_oid, PyRep::Integer(macho_client_b), st_ref]));
    assert!(undock_disc.contains(&vec![rp_oid, PyRep::Integer(macho_client_b), rp_ref]));

    let (cr_bid, cr_oid, cr_ref) = macho_bind(&ctx, &mut sess_b, "corpRegistry", 1_000_009).await;
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(&ctx, &mut sess_b, "corpRegistry", "UnBind", &[PyRep::Integer(cr_bid as i64)]).await.expect("UnBind");
    let _ = sess_b.drain_notifications();
    let unbind_disc: Vec<_> = ctx.drain_notifications_for_char(PILOT_B).into_iter().filter(|n| n.event == "OnMachoObjectDisconnect").map(|n| n.args).collect();
    assert_eq!(unbind_disc, vec![vec![cr_oid, PyRep::Integer(macho_client_b), cr_ref]]);

    // Pod Pilot C via weapon fire with bound beyonce object -> 3-arg OnMachoObjectDisconnect
    let (_pc_bid, pc_oid, pc_ref) = macho_bind(&ctx, &mut sess_c, "beyonce", SYS_JITA as i64).await;
    let mut pod_cs = ctx.build_ship_combat_state(PILOT_C, pod_c, 670);
    pod_cs.is_cloaked = false;
    pod_cs.gate_cloak_remaining_ticks = 0;
    pod_cs.undock_invulnerable_remaining_ticks = 0;
    pod_cs.shield_hp = 0.0;
    pod_cs.armor_hp = 0.0;
    pod_cs.structure_hp = 1.0;
    let _ = jita
        .spawn_ball_with_meta(
            Ball {
                id: pod_c, type_id: 670, position: DVec3::new(2_500.0, 0.0, 0.0), velocity: DVec3::ZERO,
                max_velocity: 200.0, inertia: 1.0, mass: 50_000.0, radius: 25.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            pod_cs,
            ctx.build_ship_slim_meta(PILOT_C, 670),
        )
        .await;
    lock_target(&ctx, &mut sess_a, SYS_JITA, ship_a, pod_c).await;
    let _ = drain_all(&ctx, &mut sess_c, PILOT_C);
    let mut pod_notifs = Vec::new();
    for _ in 0..8 {
        rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(a_gun_id), PyRep::String("projectileFired".into()), PyRep::Integer(pod_c)])
            .await
            .expect("fire at pod_c");
        pod_notifs.extend(drain_all(&ctx, &mut sess_c, PILOT_C));
        if pod_notifs.iter().any(|(e, _)| e == "OnMachoObjectDisconnect") {
            break;
        }
        ctx.advance_secs(5);
    }
    let disc_args = find_notif(&pod_notifs, "OnMachoObjectDisconnect");
    let macho_client_c = (1_000_000_000_000_u64 + sess_c.session_id) as i64;
    assert_eq!(disc_args, &[pc_oid, PyRep::Integer(macho_client_c), pc_ref]);

    // 12. OnWormholeJumpCancel (0-arg) across out-of-range, no-destination, and polarized branches
    let wh_ball_id = 710_000_199_i64;
    let _ = jita
        .spawn_ball_with_meta(
            Ball {
                id: wh_ball_id,
                type_id: 26272,
                position: DVec3::new(50_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            Default::default(),
            SlimItemMeta {
                group_id: 988,
                category_id: 2,
                owner_id: 0,
                char_id: None,
                corp_id: -1,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false, ..Default::default()
            },
        )
        .await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.get_or_create(PILOT_A);
        st.criminal_flag_until_unix = 0;
        st.pending_concord_arrival = None;
    }
    let wh_err = rpc(&ctx, &mut sess_a, "wormholeMgr", "WormholeJump", &[PyRep::Integer(wh_ball_id)])
        .await
        .expect_err("wormhole outside range must fail");
    assert_eq!(wh_err.code, "OutsideWormholeJumpRange");
    assert_eq!(find_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnWormholeJumpCancel"), &[]);

    // Move ship_a within 2500m of wormhole and test NoWormholeDestination + WormholePolarized without deadlock
    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_a).await {
        sb.position = DVec3::new(49_500.0, 0.0, 0.0);
        let _ = jita.spawn_ball(sb).await;
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&SYS_JITA) {
            st.wormhole_destination_system_id = None;
        }
    });
    let no_dest_err = rpc(&ctx, &mut sess_a, "wormholeMgr", "WormholeJump", &[PyRep::Integer(wh_ball_id)])
        .await
        .expect_err("wormhole without destination must fail");
    assert_eq!(no_dest_err.code, "NoWormholeDestination");
    assert_eq!(find_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnWormholeJumpCancel"), &[]);
    assert!(ctx.db.with_snapshot(|s| s.characters.contains_key(&PILOT_A)));

    let pol_now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(SYS_JITA).or_default().wormhole_destination_system_id = Some(30_000_144);
        snap.wh_polarizations.insert((PILOT_A, 30_000_144), pol_now + 300);
    });
    let pol_err = rpc(&ctx, &mut sess_a, "wormholeMgr", "WormholeJump", &[PyRep::Integer(wh_ball_id)])
        .await
        .expect_err("polarized wormhole jump must fail");
    assert_eq!(pol_err.code, "WormholePolarized");
    assert_eq!(find_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnWormholeJumpCancel"), &[]);
    assert!(ctx.db.with_snapshot(|s| s.characters.contains_key(&PILOT_A)));

    // 13. OnRemoteMessage (2-arg) via GM /heal slash command
    sess_a.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String("/heal".into())])
        .await
        .expect("GM /heal");
    let heal_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let remote_args = find_notif(&heal_notifs, "OnRemoteMessage");
    assert_eq!(remote_args.len(), 2);
    assert_eq!(remote_args[0], PyRep::String("CustomInfo".into()));
    assert!(remote_args[1].dict_get("info").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()));
}

// Folded submodule: notify_real_emitters_batch7_part2
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch7_part2 {
    use super::*;

// Real server-side emission tests for Notify Batch 7 (Part 2: Fitting, Weapon Banks,
// Drones, Stances, Conduit Jump, Character Energy, Skill Injectors & Skill Bundles).

use crate::notify_real_emitters_batch7::{
    drain_all, find_notif, login_in_space, make_item, new_ctx, rpc, JITA_STATION, PILOT_A,
    PILOT_B, SYS_JITA,
};
use eve_db::{FleetMemberRecord, FleetRecord, FleetRole, InventoryFlag, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::{HashMap, HashSet};

// parity: notify:OnWeaponBanksChanged, notify:OnWeaponGroupDestroyed
// parity: notify:OnFittingAdded, notify:OnFittingDeleted, notify:OnManyFittingsDeleted
// parity: notify:OnDroneStateChange, notify:OnStanceActive
// parity: notify:OnGroupJumpAnchorActivated, notify:OnGroupJumpedPassenger
// parity: notify:OnNonDiminishingInjectionsUsed
// DATA GAP: OnCharacterEnergyChanged (no V24 gameplay CEQ consumer yet) and OnSkillBundleInjected (skill bundle definitions absent from V24 client FSD) remain open.
#[tokio::test]
async fn notify_batch7_weapons_fittings_drones_stances_jump_and_skills() {
    eve_testkit::attest(&[
        "notify:OnWeaponBanksChanged",
        "notify:OnWeaponGroupDestroyed",
        "notify:OnFittingAdded",
        "notify:OnFittingDeleted",
        "notify:OnManyFittingsDeleted",
        "notify:OnDroneStateChange",
        "notify:OnStanceActive",
        "notify:OnGroupJumpAnchorActivated",
        "notify:OnGroupJumpedPassenger",
        "notify:OnNonDiminishingInjectionsUsed",
    ]);

    let ctx = new_ctx();
    let (ship_a, ship_b) = (720_000_001_i64, 720_000_002_i64);
    let mut sess_a = login_in_space(&ctx, 7_201, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;
    let mut sess_b = login_in_space(&ctx, 7_202, "other", "other-secret", PILOT_B, ship_b, 621, SYS_JITA, DVec3::new(1_000.0, 0.0, 0.0)).await;

    // 1-2. OnWeaponBanksChanged (2-arg) & OnWeaponGroupDestroyed (2-arg)
    let (g1, g2) = (720_000_011_i64, 720_000_012_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(g1, make_item(g1, 2873, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(g2, make_item(g2, 2873, PILOT_A, ship_a, InventoryFlag::HiSlot1, 1, true));
    });
    ctx.invalidate_ship_fitting(ship_a);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    rpc(&ctx, &mut sess_a, "dogmaIM", "LinkWeapons", &[PyRep::Integer(ship_a), PyRep::Integer(g1), PyRep::Integer(g2)])
        .await
        .expect("LinkWeapons");
    let link_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let banks_args = find_notif(&link_notifs, "OnWeaponBanksChanged");
    assert_eq!(banks_args.len(), 2);
    assert_eq!(banks_args[0], PyRep::Integer(ship_a));
    assert!(matches!(&banks_args[1], PyRep::Dict(entries) if entries.iter().any(|(k, _)| *k == PyRep::Integer(g1))));

    rpc(&ctx, &mut sess_a, "dogmaIM", "DestroyWeaponBank", &[PyRep::Integer(ship_a), PyRep::Integer(g1)])
        .await
        .expect("DestroyWeaponBank");
    let destroy_bank_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&destroy_bank_notifs, "OnWeaponGroupDestroyed"), &[PyRep::Integer(ship_a), PyRep::Integer(g1)]);

    // 3-5. OnFittingAdded (2-arg), OnFittingDeleted (2-arg), OnManyFittingsDeleted (2-arg)
    let fit_payload = |name: &str| {
        PyRep::keyval([
            ("name", PyRep::String(name.into())),
            ("shipTypeID", PyRep::Integer(587)),
            ("description", PyRep::String("Batch 7 fit".into())),
            ("fitData", PyRep::List(Vec::new())),
        ])
    };
    let save1 = rpc(&ctx, &mut sess_a, "charFittingMgr", "SaveFitting", &[PyRep::Integer(PILOT_A), fit_payload("Fit One")])
        .await
        .expect("SaveFitting 1");
    let fit1 = save1.as_i64().expect("fit1");
    let add_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&add_notifs, "OnFittingAdded"), &[PyRep::Integer(PILOT_A), PyRep::Integer(fit1)]);

    let save2 = rpc(&ctx, &mut sess_a, "charFittingMgr", "SaveFitting", &[PyRep::Integer(PILOT_A), fit_payload("Fit Two")])
        .await
        .expect("SaveFitting 2");
    let fit2 = save2.as_i64().expect("fit2");
    let save3 = rpc(&ctx, &mut sess_a, "charFittingMgr", "SaveFitting", &[PyRep::Integer(PILOT_A), fit_payload("Fit Three")])
        .await
        .expect("SaveFitting 3");
    let fit3 = save3.as_i64().expect("fit3");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    rpc(&ctx, &mut sess_a, "charFittingMgr", "DeleteFitting", &[PyRep::Integer(PILOT_A), PyRep::Integer(fit1)])
        .await
        .expect("DeleteFitting");
    let del_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&del_notifs, "OnFittingDeleted"), &[PyRep::Integer(PILOT_A), PyRep::Integer(fit1)]);

    rpc(
        &ctx,
        &mut sess_a,
        "charFittingMgr",
        "DeleteManyFittings",
        &[PyRep::Integer(PILOT_A), PyRep::List(vec![PyRep::Integer(fit2), PyRep::Integer(fit3)])],
    )
    .await
    .expect("DeleteManyFittings");
    let del_many_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let many_args = find_notif(&del_many_notifs, "OnManyFittingsDeleted");
    assert_eq!(many_args.len(), 2);
    assert_eq!(many_args[0], PyRep::Integer(PILOT_A));
    assert_eq!(many_args[1], PyRep::List(vec![PyRep::Integer(fit2), PyRep::Integer(fit3)]));

    // 6. OnDroneStateChange (7-arg) via ship.LaunchDrones
    let drone_id = 720_000_021_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_a) {
            sh.type_id = 645; // Dominix
        }
        snap.items.insert(drone_id, make_item(drone_id, 2454, PILOT_A, ship_a, InventoryFlag::DroneBay, 1, false));
        snap.skills.entry(PILOT_A).or_default().insert(
            3436,
            TrainedSkillRecord { type_id: 3436, level: 5, skill_points: 256_000.0 },
        );
    });
    ctx.invalidate_ship_fitting(ship_a);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let launch_rep = rpc(&ctx, &mut sess_a, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(drone_id)])])
        .await
        .expect("LaunchDrones");
    let launched_ball_id = match launch_rep {
        PyRep::List(v) => v.first().and_then(|x| x.as_i64()).expect("launched drone ball"),
        _ => panic!("expected List from LaunchDrones"),
    };
    assert!(launched_ball_id > 0);
    let wire_drone_notifs = sess_a.drain_notifications();
    let ctx_drone_notifs = ctx.drain_notifications_for_char(PILOT_A);
    let wire_drone_events: Vec<&[PyRep]> = wire_drone_notifs
        .iter()
        .filter(|(ev, _)| ev == "OnDroneStateChange")
        .map(|(_, args)| args.as_slice())
        .collect();
    assert_eq!(wire_drone_events.len(), 1, "LaunchDrones must push exactly one OnDroneStateChange to session");
    assert_eq!(
        ctx_drone_notifs.iter().filter(|n| n.event == "OnDroneStateChange").count(),
        1,
        "LaunchDrones must enqueue exactly one OnDroneStateChange in ctx"
    );
    let drone_args = wire_drone_events[0];
    assert_eq!(drone_args.len(), 7);
    assert_eq!(drone_args[0], PyRep::Integer(launched_ball_id));
    assert_eq!(drone_args[1], PyRep::Integer(PILOT_A));
    assert_eq!(drone_args[2], PyRep::Integer(ship_a));
    assert_eq!(drone_args[3], PyRep::Integer(0));
    assert_eq!(drone_args[4], PyRep::Integer(2454));
    assert_eq!(drone_args[5], PyRep::Integer(PILOT_A));
    assert_eq!(drone_args[6], PyRep::None);

    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "ChangeDroneSettings",
        &[PyRep::Dict(vec![
            (PyRep::String("aggressive".into()), PyRep::Bool(true)),
            (PyRep::String("focusFire".into()), PyRep::Bool(false)),
        ])],
    )
    .await
    .expect("ChangeDroneSettings");
    let wire_setting_notifs = sess_a.drain_notifications();
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    assert_eq!(find_notif(&wire_setting_notifs, "OnDroneSettingChanged"), &[PyRep::Bool(true), PyRep::Bool(false)]);
    let post_setting_drone_events: Vec<&[PyRep]> = wire_setting_notifs
        .iter()
        .filter(|(ev, _)| ev == "OnDroneStateChange")
        .map(|(_, args)| args.as_slice())
        .collect();
    assert_eq!(post_setting_drone_events.len(), 1);
    assert_eq!(post_setting_drone_events[0].len(), 7);
    assert_eq!(post_setting_drone_events[0][0], PyRep::Integer(launched_ball_id));
    assert_eq!(post_setting_drone_events[0][6], PyRep::None);

    // 7. OnStanceActive (2-arg) via shipStanceMgr.SetShipStance on Confessor (34317)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_a) {
            sh.type_id = 34317; // Confessor
        }
    });
    ctx.invalidate_ship_fitting(ship_a);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(&ctx, &mut sess_a, "shipStanceMgr", "SetShipStance", &[PyRep::Integer(ship_a), PyRep::Integer(34319)])
        .await
        .expect("SetShipStance");
    let stance_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&stance_notifs, "OnStanceActive"), &[PyRep::Integer(ship_a), PyRep::Integer(34319)]);

    // 8-9. OnGroupJumpAnchorActivated (2-arg) & OnGroupJumpedPassenger (2-arg)
    let (sys_origin, sys_dest) = {
        let lowsec: Vec<(u32, [f64; 3])> = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| s.security_status > 0.0 && s.security_status < 0.45 && s.solar_system_id < 31_000_000 && s.galactic_pos_ly != [0.0; 3])
            .map(|s| (s.solar_system_id, s.galactic_pos_ly))
            .collect();
        let mut pair = (30_002_813_u32, 30_002_814_u32);
        'outer: for &(a, pa) in &lowsec {
            for &(b, pb) in &lowsec {
                if a != b {
                    let d = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt();
                    if (0.3..3.0).contains(&d) {
                        pair = (a, b);
                        break 'outer;
                    }
                }
            }
        }
        pair
    };
    let (beacon_item_id, beacon_owner, fleet_id) = (720_000_051_i64, 99_999_i64, 720_601_i64);
    ctx.db.with_mut_snapshot(|snap| {
        for &sk in &[20533_u32, 21611, 21610, 24311, 24312] {
            snap.skills.entry(PILOT_A).or_default().insert(sk, TrainedSkillRecord { type_id: sk, level: 5, skill_points: 256_000.0 });
        }
        snap.skills.entry(PILOT_B).or_default().insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = sys_origin;
            ch.jump_cooldown_until_unix = 0;
        }
        if let Some(ch) = snap.characters.get_mut(&PILOT_B) {
            ch.solar_system_id = sys_origin;
            ch.jump_cooldown_until_unix = 0;
        }
        if let Some(sh) = snap.items.get_mut(&ship_a) {
            sh.type_id = 23757; // Archon
            sh.location_id = sys_origin as i64;
        }
        if let Some(sh) = snap.items.get_mut(&ship_b) {
            sh.type_id = 621; // Caracal
            sh.location_id = sys_origin as i64;
        }
        snap.items.insert(720_000_052, make_item(720_000_052, 16274, PILOT_A, ship_a, InventoryFlag::Cargo, 100_000, false));
        snap.items.insert(beacon_item_id, make_item(beacon_item_id, 56099, beacon_owner, sys_dest as i64, InventoryFlag::None, 1, true));
        let mut member_details = HashMap::new();
        for (cid, role, wid, sid) in [
            (PILOT_A, FleetRole::FleetCommander, None, None),
            (PILOT_B, FleetRole::SquadMember, Some(1), Some(1)),
            (beacon_owner, FleetRole::SquadMember, Some(1), Some(1)),
        ] {
            member_details.insert(
                cid,
                FleetMemberRecord {
                    character_id: cid,
                    role,
                    wing_id: wid,
                    squad_id: sid,
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: format!("{role:?}"),
                    join_time_unix: 1,
                },
            );
        }
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: PILOT_A,
                members: vec![PILOT_A, PILOT_B, beacon_owner],
                wings: vec![1],
                squads: vec![1],
                free_move: true,
                is_registered: false,
                wing_details: HashMap::new(),
                squad_details: HashMap::new(),
                member_details,
                invites: HashSet::new(),
                motd: String::new(),
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_a);
    ctx.invalidate_ship_fitting(ship_b);
    sess_a.solar_system_id = Some(sys_origin);
    sess_a.fleet_id = Some(fleet_id);
    sess_b.solar_system_id = Some(sys_origin);
    sess_b.fleet_id = Some(fleet_id);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_A);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_B);
    let orig_h = ctx.systems.get_or_spawn(sys_origin);
    for (cid, sid, tid, pos) in [
        (PILOT_A, ship_a, 23757_u32, DVec3::ZERO),
        (PILOT_B, ship_b, 621_u32, DVec3::new(1_000.0, 0.0, 0.0)),
    ] {
        let mut cs = ctx.build_ship_combat_state(cid, sid, tid);
        cs.is_cloaked = false;
        cs.gate_cloak_remaining_ticks = 0;
        cs.undock_invulnerable_remaining_ticks = 0;
        let _ = orig_h
            .spawn_ball_with_meta(
                Ball {
                    id: sid,
                    type_id: tid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 1.0,
                    mass: 10_000_000.0,
                    radius: 100.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
                ctx.build_ship_slim_meta(cid, tid),
            )
            .await;
    }
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    rpc(
        &ctx,
        &mut sess_a,
        "beyonce",
        "CmdGroupJumpToFleetDeployableBeacon",
        &[PyRep::Integer(beacon_owner), PyRep::Integer(sys_dest as i64), PyRep::Integer(beacon_item_id)],
    )
    .await
    .expect("CmdGroupJumpToFleetDeployableBeacon");
    let anchor_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&anchor_notifs, "OnGroupJumpAnchorActivated"),
        &[PyRep::Integer(sys_dest as i64), PyRep::Integer(1)]
    );
    let pass_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(
        find_notif(&pass_notifs, "OnGroupJumpedPassenger"),
        &[PyRep::Integer(PILOT_A), PyRep::Integer(sys_dest as i64)]
    );

    // 10. GM /energy input validation + characterEnergyMgr state check (DATA GAP: OnCharacterEnergyChanged remains open without gameplay CEQ consumer)
    sess_a.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    assert_eq!(
        rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String("/energy bogus 100".into())])
            .await
            .expect_err("invalid char_id must fail")
            .code,
        "InvalidArguments"
    );
    assert_eq!(
        rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String("/energy 999999999 100".into())])
            .await
            .expect_err("nonexistent char_id must fail")
            .code,
        "CharacterNotFound"
    );
    rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String("/energy 40".into())])
        .await
        .expect("GM /energy");
    let energy_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let energy_args = find_notif(&energy_notifs, "OnCharacterEnergyChanged");
    assert_eq!(energy_args.len(), 2);
    assert_eq!(energy_args[0], PyRep::Integer(PILOT_A));
    assert_eq!(energy_args[1].dict_get("currentEnergy").and_then(|v| v.as_i64()), Some(40));

    // 11. OnNonDiminishingInjectionsUsed (1-arg) + crateService.ClaimSkillBundle (DATA GAP: OnSkillBundleInjected remains open)
    let (mini_inj_id, crate_id) = (720_000_071_i64, 720_000_072_i64);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(JITA_STATION);
            ch.solar_system_id = SYS_JITA;
        }
        snap.items.insert(mini_inj_id, make_item(mini_inj_id, 43630, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false));
        snap.items.insert(crate_id, make_item(crate_id, 53533, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false));
    });
    sess_a.station_id = Some(JITA_STATION);
    sess_a.solar_system_id = Some(SYS_JITA);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    rpc(&ctx, &mut sess_a, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(mini_inj_id)])
        .await
        .expect("InjectSkillpoints Mini Skill Injector");
    let inj_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&inj_notifs, "OnNonDiminishingInjectionsUsed"), &[PyRep::Integer(1)]);

    rpc(&ctx, &mut sess_a, "crateService", "OpenCrate", &[PyRep::Integer(crate_id)])
        .await
        .expect("OpenCrate 53533");
    let claim_rep = rpc(&ctx, &mut sess_a, "crateService", "ClaimSkillBundle", &[PyRep::Integer(crate_id), PyRep::Integer(39)])
        .await
        .expect("ClaimSkillBundle");
    assert_eq!(claim_rep.dict_get("spGranted").and_then(|v| v.as_f64()), Some(25_000.0));
    let bundle_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(
        !bundle_notifs.iter().any(|(e, _)| e == "OnSkillBundleInjected"),
        "OnSkillBundleInjected must not be emitted with hardcoded skill count until skill bundle definitions exist"
    );
}

}

// Folded submodule: notify_real_emitters_batch7_part3
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch7_part3 {
    use super::*;

// Real server-side emission tests for Notify Batch 7 (Part 3: Cosmetics, Petitions,
// Mail Undelete, Planetary Interaction Launches, Orbital State & Escalations).

use crate::notify_real_emitters_batch7::{
    drain_all, find_notif, login_in_space, make_item, new_ctx, rpc, JITA_STATION, PILOT_A,
    PILOT_B, SYS_JITA,
};
use crate::quasar_goal_tale_notices::{seq, subscribe};
use crate::quasar_progression_complete::{build_sess, quasar_rpc};
use eve_db::{InventoryFlag, IskCents, TrainedSkillRecord};
use eve_destiny::DamageVector;
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::services::pve_exploration::with_pve_exploration_reg;
use glam::DVec3;

// parity: notify:OnSkinLicenseActivated, notify:OnSkinLicenseRemoved
// parity: notify:OnShipCosmeticChanged, notify:OnShipCosmeticsChanged
// parity: notify:OnNewPetition, notify:OnAssignPetition
// parity: notify:OnPetitionMessage, notify:OnClosePetition
// parity: notify:OnMailUndeleted, notify:OnPILaunchesChange
// parity: notify:OnMajorPlanetStateUpdate, notify:OnEscalatingPathMessage
#[tokio::test]
async fn notify_batch7_cosmetics_petitions_mail_pi_and_escalations() {
    eve_testkit::attest(&[
        "notify:OnSkinLicenseActivated",
        "notify:OnSkinLicenseRemoved",
        "notify:OnShipCosmeticChanged",
        "notify:OnShipCosmeticsChanged",
        "notify:OnNewPetition",
        "notify:OnAssignPetition",
        "notify:OnPetitionMessage",
        "notify:OnClosePetition",
        "notify:OnMailUndeleted",
        "notify:OnPILaunchesChange",
        "notify:OnMajorPlanetStateUpdate",
        "notify:OnEscalatingPathMessage",
    ]);

    let ctx = new_ctx();
    let (ship_a, ship_b) = (730_000_001_i64, 730_000_002_i64);
    let mut sess_a = login_in_space(&ctx, 7_301, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;
    let mut sess_b = login_in_space(&ctx, 7_302, "other", "other-secret", PILOT_B, ship_b, 587, SYS_JITA, DVec3::new(1_000.0, 0.0, 0.0)).await;

    // 1-2. OnSkinLicenseActivated (2-arg) & OnSkinLicenseRemoved (3-arg on timed SKIN expiry)
    let (perm_lic_tid, timed_lic_tid) = (34_748_u32, 35_493_u32);
    let perm_skin_id = ctx.sde.skin_licenses.get(&perm_lic_tid).expect("34748 in SDE").skin_id;
    let timed_lic = ctx.sde.skin_licenses.get(&timed_lic_tid).expect("35493 in SDE");
    let (timed_skin_id, timed_days) = (timed_lic.skin_id, timed_lic.duration);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(JITA_STATION);
        }
        snap.items.insert(730_000_011, make_item(730_000_011, perm_lic_tid, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false));
        snap.items.insert(730_000_012, make_item(730_000_012, timed_lic_tid, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false));
    });
    sess_a.station_id = Some(JITA_STATION);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    rpc(&ctx, &mut sess_a, "shipCosmeticsMgr", "ActivateSkinLicense", &[PyRep::Integer(perm_lic_tid as i64)])
        .await
        .expect("Activate permanent SKIN");
    let skin_act_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&skin_act_notifs, "OnSkinLicenseActivated"),
        &[PyRep::Integer(perm_skin_id as i64), PyRep::Integer(PILOT_A)]
    );

    rpc(&ctx, &mut sess_a, "shipCosmeticsMgr", "ActivateSkinLicense", &[PyRep::Integer(timed_lic_tid as i64)])
        .await
        .expect("Activate timed SKIN");
    rpc(&ctx, &mut sess_a, "shipCosmeticsMgr", "ApplySkinToShip", &[PyRep::Integer(ship_a), PyRep::Integer(timed_lic_tid as i64)])
        .await
        .expect("Apply timed SKIN to ship");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    ctx.advance_secs((timed_days as u64 + 1) * 86_400);
    rpc(&ctx, &mut sess_a, "shipCosmeticsMgr", "GetLicencedSkins", &[])
        .await
        .expect("GetLicencedSkins after expiry");
    let skin_rm_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&skin_rm_notifs, "OnSkinLicenseRemoved"),
        &[PyRep::Integer(timed_skin_id as i64), PyRep::Integer(ship_a), PyRep::Integer(587)]
    );

    // 3-4. OnShipCosmeticChanged (3-arg) & OnShipCosmeticsChanged (2-arg) via Quasar ship logo DisplayRequest
    let corp_a = ctx.db.with_snapshot(|s| s.characters[&PILOT_A].corporation_id as u32);
    let mut gm_q = build_sess(7_399, PILOT_A as u32, corp_a);
    gm_q.user_id = Some(7_399);
    let mut player_q = build_sess(7_398, PILOT_A as u32, corp_a);
    player_q.user_id = Some(7_398);
    player_q.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    let _rx_q = subscribe(&ctx, &mut player_q).await;

    let wrap = |field: u32, payload: &[u8]| {
        let mut out = Vec::new();
        encode_bytes_field(field, payload, &mut out);
        out
    };
    let logo_ent = eve_server::services::quasar_entitlement_membership::encode_logo_identifier(PILOT_A as u64, 587);
    assert_eq!(
        quasar_rpc(&ctx, &mut gm_q, "eve_public.entitlement.character.ship.admin.corplogo.GrantRequest", wrap(1, &logo_ent))
            .await
            .status_code,
        STATUS_OK
    );
    let mut id_bytes = Vec::new();
    seq(1, ship_a as u64, &mut id_bytes);
    encode_varint_field(2, 0, &mut id_bytes);
    let mut logo_req = wrap(1, &id_bytes);
    encode_bytes_field(2, &wrap(2, &[]), &mut logo_req);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        quasar_rpc(&ctx, &mut player_q, "eve_public.cosmetic.ship.logo.DisplayRequest", logo_req)
            .await
            .status_code,
        STATUS_OK
    );
    let cos_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let cos_single = find_notif(&cos_notifs, "OnShipCosmeticChanged");
    assert_eq!(cos_single, &[PyRep::Integer(ship_a), PyRep::Integer(0), PyRep::Integer(1)]);
    let cos_map = find_notif(&cos_notifs, "OnShipCosmeticsChanged");
    assert_eq!(
        cos_map,
        &[PyRep::Integer(ship_a), PyRep::Dict(vec![(PyRep::Integer(0), PyRep::Integer(1))])]
    );

    // 5-8. OnNewPetition (1-arg), OnAssignPetition (2-arg), OnPetitionMessage (4-arg), OnClosePetition (1-arg)
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let pet_rep = rpc(
        &ctx,
        &mut sess_a,
        "petitioner",
        "CreatePetition",
        &[PyRep::String("Batch 7 Ticket".into()), PyRep::String("Need assistance".into()), PyRep::Integer(1)],
    )
    .await
    .expect("CreatePetition");
    let pet_id = pet_rep.dict_get("petitionID").and_then(|v| v.as_i64()).expect("petitionID");
    let new_pet_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&new_pet_notifs, "OnNewPetition"), &[PyRep::Integer(pet_id)]);

    sess_b.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    rpc(&ctx, &mut sess_b, "petitioner", "ClaimPetition", &[PyRep::Integer(pet_id)])
        .await
        .expect("ClaimPetition");
    let claim_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&claim_notifs, "OnAssignPetition"), &[PyRep::Integer(pet_id), PyRep::Integer(PILOT_B)]);

    rpc(&ctx, &mut sess_a, "petitioner", "PetitionerChat", &[PyRep::Integer(pet_id), PyRep::String("Follow-up info".into())])
        .await
        .expect("PetitionerChat");
    let msg_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let msg_args = find_notif(&msg_notifs, "OnPetitionMessage");
    assert_eq!(msg_args.len(), 4);
    assert_eq!(msg_args[0], PyRep::Integer(pet_id));
    assert_eq!(msg_args[1], PyRep::Integer(PILOT_A));
    assert_eq!(msg_args[2], PyRep::String("Follow-up info".into()));
    assert!(msg_args[3].as_i64().unwrap_or(0) > 0);

    rpc(&ctx, &mut sess_b, "petitioner", "ClosePetition", &[PyRep::Integer(pet_id)])
        .await
        .expect("ClosePetition");
    let close_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&close_notifs, "OnClosePetition"), &[PyRep::Integer(pet_id)]);

    // 9. OnMailUndeleted (1-arg list of messageIDs) via MoveFromTrash
    let mail_rep = rpc(
        &ctx,
        &mut sess_a,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(PILOT_B)]),
            PyRep::Integer(0),
            PyRep::None,
            PyRep::String("Batch 7 Mail".into()),
            PyRep::String("Restore test".into()),
        ],
    )
    .await
    .expect("SendMail");
    let mail_id = mail_rep.as_i64().expect("mail_id");
    rpc(&ctx, &mut sess_b, "mailMgr", "MoveToTrash", &[PyRep::Integer(mail_id)])
        .await
        .expect("MoveToTrash");
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(&ctx, &mut sess_b, "mailMgr", "MoveFromTrash", &[PyRep::Integer(mail_id)])
        .await
        .expect("MoveFromTrash");
    let mail_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(find_notif(&mail_notifs, "OnMailUndeleted"), &[PyRep::List(vec![PyRep::Integer(mail_id)])]);

    // 10-11. OnPILaunchesChange (0-arg) & OnMajorPlanetStateUpdate (2-arg)
    let jita_planet_id = 40_009_077_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = None;
            ch.solar_system_id = SYS_JITA;
            ch.balance = IskCents::from_isk(100_000_000);
        }
        let sk = snap.skills.entry(PILOT_A).or_default();
        sk.insert(2505, TrainedSkillRecord { type_id: 2505, level: 5, skill_points: 256_000.0 });
        sk.insert(2495, TrainedSkillRecord { type_id: 2495, level: 4, skill_points: 256_000.0 });
        snap.items.insert(730_000_041, make_item(730_000_041, 2524, PILOT_A, ship_a, InventoryFlag::Cargo, 1, false));
        snap.items.insert(730_000_042, make_item(730_000_042, 2389, PILOT_A, ship_a, InventoryFlag::Cargo, 500, false));
    });
    sess_a.station_id = None;
    sess_a.solar_system_id = Some(SYS_JITA);
    sess_a.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(
        &ctx,
        &mut sess_a,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(jita_planet_id),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(1001), PyRep::Integer(2524), PyRep::Real(0.10), PyRep::Real(0.10)]),
            ]),
        ],
    )
    .await
    .expect("Deploy PI colony");
    let deploy_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&deploy_notifs, "OnMajorPlanetStateUpdate"),
        &[PyRep::Integer(jita_planet_id), PyRep::Bool(true)]
    );

    rpc(
        &ctx,
        &mut sess_a,
        "planetMgr",
        "UserLaunchCommodities",
        &[PyRep::Integer(jita_planet_id), PyRep::Integer(2389), PyRep::Integer(300), PyRep::String("ImportToColony".into())],
    )
    .await
    .expect("ImportToColony");
    let import_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!import_notifs.iter().any(|(e, _)| e == "OnPILaunchesChange"));

    let launch_rep = rpc(
        &ctx,
        &mut sess_a,
        "planetMgr",
        "UserLaunchCommodities",
        &[PyRep::Integer(jita_planet_id), PyRep::Integer(2389), PyRep::Integer(100), PyRep::String("CustomsOffice".into())],
    )
    .await
    .expect("Launch to CustomsOffice");
    let launch_id = launch_rep.dict_get("launchID").and_then(|v| v.as_i64()).expect("launchID");
    let pi_launch_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&pi_launch_notifs, "OnPILaunchesChange"), &[]);

    rpc(&ctx, &mut sess_a, "planetMgr", "DeleteLaunch", &[PyRep::Integer(launch_id)])
        .await
        .expect("DeleteLaunch");
    let pi_del_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&pi_del_notifs, "OnPILaunchesChange"), &[]);

    sess_a.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    rpc(&ctx, &mut sess_a, "planetOrbitalRegistryBroker", "UpdateSettings", &[PyRep::Integer(jita_planet_id)])
        .await
        .expect("UpdateSettings");
    let orb_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&orb_notifs, "OnMajorPlanetStateUpdate"),
        &[PyRep::Integer(jita_planet_id), PyRep::Bool(false)]
    );

    rpc(&ctx, &mut sess_a, "planetMgr", "UserAbandonPlanet", &[PyRep::Integer(jita_planet_id)])
        .await
        .expect("UserAbandonPlanet");
    let abandon_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&abandon_notifs, "OnMajorPlanetStateUpdate"),
        &[PyRep::Integer(jita_planet_id), PyRep::Bool(true)]
    );

    // 12. OnEscalatingPathMessage (1-arg instanceID) via combat anomaly escalation
    let _ = rpc(&ctx, &mut sess_a, "scanSvc", "GetCombatAnomalyInstances", &[PyRep::Integer(SYS_JITA as i64)])
        .await
        .expect("GetCombatAnomalyInstances");
    let site_id = "PVE-ANOM-30000142-1";
    async fn clear_wave(ctx: &eve_server::router::ServerContext, sid: &'static str) {
        let ball_ids = with_pve_exploration_reg(ctx, |reg| {
            reg.sites_by_system
                .get(&SYS_JITA)
                .and_then(|l| l.iter().find(|s| s.site_id == sid))
                .map(|s| s.active_npc_ball_ids.clone())
                .unwrap_or_default()
        });
        let jita = ctx.systems.get_or_spawn(SYS_JITA);
        for bid in ball_ids {
            let _ = jita
                .apply_damage(bid, DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 })
                .await;
        }
    }
    clear_wave(&ctx, site_id).await;
    rpc(&ctx, &mut sess_a, "scanSvc", "EnterCombatAnomaly", &[PyRep::String(site_id.into()), PyRep::Bool(true)])
        .await
        .expect("EnterCombatAnomaly wave 2");
    clear_wave(&ctx, site_id).await;
    // Seed 3 gives SplitMix64 first draw 0.1135 < 0.20 (natural 20% escalation roll)
    ctx.set_mechanic_rng_seed(Some(3));
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(&ctx, &mut sess_a, "scanSvc", "EnterCombatAnomaly", &[PyRep::String(site_id.into()), PyRep::Bool(true)])
        .await
        .expect("Complete anomaly with escalation");
    let pending_esc: Vec<_> = ctx
        .drain_notifications_for_char(PILOT_A)
        .into_iter()
        .filter(|n| n.event == "OnEscalatingPathMessage")
        .collect();
    let sess_esc: Vec<_> = sess_a
        .drain_notifications()
        .into_iter()
        .filter(|(e, _)| e == "OnEscalatingPathMessage")
        .collect();
    assert_eq!(pending_esc.len(), 1);
    assert_eq!(sess_esc.len(), 1);
    assert_eq!(pending_esc[0].args.len(), 1);
    assert!(pending_esc[0].args[0].as_i64().unwrap_or(0) > 0);
    assert_eq!(sess_esc[0].1, pending_esc[0].args);
}

}

// Folded submodule: notify_real_emitters_batch7_part4
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch7_part4 {
    use super::*;

// Real server-side emission tests for Notify Batch 7 (Part 4: Agent Missions,
// Faction Warfare, Sovereignty Dev Index, PVP Filaments & Tournaments).

use crate::notify_real_emitters_batch7::{
    drain_all, find_notif, login_in_space, make_item, new_ctx, rpc, PILOT_A, PILOT_B, SYS_JITA,
};
use eve_db::{FwSystemOccupationRecord, InventoryFlag};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_proto::PyRep;
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;

// parity: notify:OnAgentMissionObjectivesCompleted, notify:OnAgentMissionObjectivesFailed
// parity: notify:OnFacWarDungeonCompleted, notify:OnSolarSystemDevIndexChanged
// parity: notify:OnPVPFilamentsCharacterStatistics, notify:OnPVPFilamentsLeaderboard
// parity: notify:OnTournamentPerformBan
// DATA GAP: OnAbyssalLocationEntered, OnAbyssalContentFinished, OnVoidSpaceEntered, OnVoidSpaceRelog, OnVoidSpaceContentFinished remain open (V24 room-index location_id & weather/tier nebula selection pending).
#[tokio::test]
async fn notify_batch7_abyssal_void_missions_fw_sov_pvp_and_tourney() {
    eve_testkit::attest(&[
        "notify:OnAgentMissionObjectivesCompleted",
        "notify:OnAgentMissionObjectivesFailed",
        "notify:OnFacWarDungeonCompleted",
        "notify:OnSolarSystemDevIndexChanged",
        "notify:OnPVPFilamentsCharacterStatistics",
        "notify:OnPVPFilamentsLeaderboard",
        "notify:OnTournamentPerformBan",
    ]);

    let ctx = new_ctx();
    let (ship_a, ship_b) = (540_000_001_i64, 540_000_002_i64);
    let mut sess_a = login_in_space(&ctx, 7_401, "demo", "demo-secret", PILOT_A, ship_a, 621, SYS_JITA, DVec3::new(0.0, 2.0e9, 0.0)).await;
    let mut sess_b = login_in_space(&ctx, 7_402, "other", "other-secret", PILOT_B, ship_b, 621, SYS_JITA, DVec3::new(1_000.0, 2.0e9, 0.0)).await;

    // 6-7. OnAgentMissionObjectivesCompleted (1-arg agentID) & OnAgentMissionObjectivesFailed (1-arg agentID)
    let (agent_id, corp_id, agent_station_id, agent_sys_id) = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id == 22 && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .map(|a| {
            let sid = a.station_id.unwrap() as i64;
            let sys = ctx.sde.stations.get(&sid).map(|st| st.solar_system_id).unwrap_or(SYS_JITA);
            (a.agent_id as i64, a.corporation_id, sid, sys)
        })
        .expect("Level 1 Distribution agent in SDE");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(agent_station_id);
            ch.solar_system_id = agent_sys_id;
            ch.standings.insert(corp_id, 3.0);
        }
        if let Some(sh) = snap.items.get_mut(&ship_a) {
            sh.location_id = agent_station_id;
            sh.flag = InventoryFlag::Hangar;
        }
    });
    sess_a.station_id = Some(agent_station_id);
    sess_a.solar_system_id = Some(agent_sys_id);

    let offer1 = rpc(&ctx, &mut sess_a, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(agent_id)])
        .await
        .expect("Offer courier mission 1");
    let mid1 = offer1.dict_get("missionID").and_then(|v| v.as_i64()).expect("mid1");
    rpc(&ctx, &mut sess_a, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid1)])
        .await
        .expect("Accept courier mission 1");
    let (dest_sid, req_tid) = ctx.db.with_snapshot(|snap| {
        let meta = eve_server::services::agents_missions::load_mission_meta(snap, PILOT_A, mid1 as u32)
            .expect("mission meta");
        (
            meta.destination_station_id.unwrap_or(agent_station_id),
            meta.required_delivery_type_id.unwrap_or(3683),
        )
    });
    let dest_sys = ctx.sde.stations.get(&dest_sid).map(|s| s.solar_system_id).unwrap_or(agent_sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(dest_sid);
            ch.solar_system_id = dest_sys;
        }
        if let Some(sh) = snap.items.get_mut(&ship_a) {
            sh.location_id = dest_sid;
        }
        for it in snap.items.values_mut() {
            if it.owner_id == PILOT_A && it.type_id == req_tid {
                it.location_id = ship_a;
                it.flag = InventoryFlag::Cargo;
            }
        }
    });
    sess_a.station_id = Some(dest_sid);
    sess_a.solar_system_id = Some(dest_sys);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    rpc(&ctx, &mut sess_a, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid1)])
        .await
        .expect("Complete courier mission 1");
    let m_done_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&m_done_notifs, "OnAgentMissionObjectivesCompleted"),
        &[PyRep::Integer(agent_id)]
    );

    // Offer + accept second mission, then quit (actionID = 11) -> OnAgentMissionObjectivesFailed
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(agent_station_id);
            ch.solar_system_id = agent_sys_id;
        }
    });
    sess_a.station_id = Some(agent_station_id);
    sess_a.solar_system_id = Some(agent_sys_id);
    let offer2 = rpc(&ctx, &mut sess_a, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(agent_id)])
        .await
        .expect("Offer mission 2");
    let mid2 = offer2.dict_get("missionID").and_then(|v| v.as_i64()).expect("mid2");
    rpc(&ctx, &mut sess_a, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid2)])
        .await
        .expect("Accept mission 2");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(&ctx, &mut sess_a, "agentMgr", "DoAction", &[PyRep::Integer(11), PyRep::Integer(mid2)])
        .await
        .expect("Quit mission 2");
    let m_fail_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&m_fail_notifs, "OnAgentMissionObjectivesFailed"),
        &[PyRep::Integer(agent_id)]
    );

    // 8. OnFacWarDungeonCompleted (2-arg [was_conquered, faction_id])
    let (enemy_sys, front_sys) = {
        let lowsec: Vec<u32> = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| s.security_status > 0.0 && s.security_status < 0.45 && s.solar_system_id < 31_000_000)
            .map(|s| s.solar_system_id)
            .collect();
        let mut pair = None;
        'fw_pair: for &f in &lowsec {
            for &nb in ctx.sde.stargates().values().filter(|g| g.from_solar_system_id == f).map(|g| &g.to_solar_system_id) {
                if lowsec.contains(&nb) && nb != f {
                    pair = Some((nb, f));
                    break 'fw_pair;
                }
            }
        }
        pair.expect("adjacent lowsec pair in SDE")
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.clear();
        snap.fw_system_occupations.insert(
            enemy_sys,
            FwSystemOccupationRecord {
                solar_system_id: enemy_sys,
                owner_faction_id: 500_004,
                occupier_faction_id: 500_004,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
        snap.fw_system_occupations.insert(
            front_sys,
            FwSystemOccupationRecord {
                solar_system_id: front_sys,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = None;
            ch.solar_system_id = front_sys;
            ch.standings.insert(500_004, 1.0);
        }
        if let Some(sh) = snap.items.get_mut(&ship_a) {
            sh.type_id = 593; // Tristan (T1 Frigate)
            sh.location_id = front_sys as i64;
            sh.flag = InventoryFlag::None;
        }
    });
    sess_a.station_id = None;
    sess_a.solar_system_id = Some(front_sys);
    rpc(&ctx, &mut sess_a, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_004)])
        .await
        .expect("Enlist Gallente Militia");
    let _ = rpc(&ctx, &mut sess_a, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[])
        .await
        .expect("Spawn FW complexes");
    let (novice_gate, novice_pos) = ctx.db.with_snapshot(|s| {
        s.fw_runtime
            .fw_complexes
            .iter()
            .find(|(_, (sys, tier, _, _, done))| *sys == front_sys && tier == "Novice" && !*done)
            .map(|(&gid, _)| (gid, DVec3::from_array(s.item_positions[&gid])))
            .expect("Novice complex in front_sys")
    });
    let front_h = ctx.systems.get_or_spawn(front_sys);
    let mut fw_cs = ctx.build_ship_combat_state(PILOT_A, ship_a, 593);
    fw_cs.is_cloaked = false;
    fw_cs.gate_cloak_remaining_ticks = 0;
    fw_cs.undock_invulnerable_remaining_ticks = 0;
    let _ = front_h
        .spawn_ball_with_meta(
            Ball {
                id: ship_a,
                type_id: 593,
                position: novice_pos + DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            fw_cs,
            ctx.build_ship_slim_meta(PILOT_A, 593),
        )
        .await;
    rpc(&ctx, &mut sess_a, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(novice_gate)])
        .await
        .expect("Activate FW complex gate");
    let _ = front_h.remove_ball(novice_gate + 500_000_000).await;
    ctx.db.with_mut_snapshot(|s| {
        s.fw_runtime.complex_guards_cleared.insert(novice_gate, true);
    });
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    ctx.advance_secs(1);
    tick_background_cluster(&ctx).await;
    ctx.advance_secs(600);
    tick_background_cluster(&ctx).await;
    let fw_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&fw_notifs, "OnFacWarDungeonCompleted"),
        &[PyRep::Bool(true), PyRep::Integer(500_004)]
    );

    // 9. OnSolarSystemDevIndexChanged (1-arg solarSystemID)
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= 0.0 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .expect("nullsec system in SDE");
    let sov_now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = null_sys;
            ch.station_id = None;
        }
        let st = snap.system_states.entry(null_sys).or_default();
        st.sov_alliance_id = Some(99_000_001);
        st.strategic_index = 0;
        snap.sov_runtime.sov_claimed_since_unix.insert(null_sys, sov_now);
    });
    sess_a.solar_system_id = Some(null_sys);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    ctx.advance_secs(8 * 86_400);
    tick_background_cluster(&ctx).await;
    let sov_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&sov_notifs, "OnSolarSystemDevIndexChanged"), &[PyRep::Integer(null_sys as i64)]);

    // 10-11. OnPVPFilamentsCharacterStatistics (1-arg Dict) & OnPVPFilamentsLeaderboard (1-arg Dict)
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = rpc(&ctx, &mut sess_a, "pvpFilamentMgr", "GetCharacterStatistics", &[PyRep::Integer(171)])
        .await
        .expect("GetCharacterStatistics");
    let _ = rpc(&ctx, &mut sess_a, "pvpFilamentMgr", "GetLeaderboard", &[PyRep::Integer(171)])
        .await
        .expect("GetLeaderboard");
    assert!(
        drain_all(&ctx, &mut sess_a, PILOT_A).is_empty(),
        "read-only GetCharacterStatistics / GetLeaderboard must not emit notifications"
    );

    ctx.advance_secs(30 * 86_400);
    let all_events = rpc(&ctx, &mut sess_a, "pvpFilamentMgr", "GetAllEvents", &[])
        .await
        .expect("GetAllEvents");
    let PyRep::Dict(ev_entries) = all_events else { panic!("expected Dict from GetAllEvents") };
    let (expected_event_id, expected_match_type_id, expected_schedule_id) = ev_entries
        .iter()
        .filter_map(|(k, v)| {
            let eid = k.as_i64()?;
            let has_72982 = match v.dict_get("typeIDs") {
                Some(PyRep::List(l)) => l.iter().any(|x| x.as_i64() == Some(72_982)),
                _ => false,
            };
            if !has_72982 {
                return None;
            }
            let mt = v.dict_get("matchTypeID")?.as_i64()?;
            let sc = v.dict_get("scheduleID")?.as_i64()?;
            Some((eid, mt, sc))
        })
        .max_by_key(|&(eid, _, _)| eid)
        .expect("event for 72982 in SDE");

    let (fil_a, fil_b, turret_id, ammo_id) = (740_000_011_i64, 740_000_012_i64, 740_000_013_i64, 740_000_014_i64);
    ctx.db.with_mut_snapshot(|snap| {
        for &(cid, sid, fid) in &[(PILOT_A, ship_a, fil_a), (PILOT_B, ship_b, fil_b)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = SYS_JITA;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = sid;
            }
            if let Some(sh) = snap.items.get_mut(&sid) {
                sh.type_id = 621; // Caracal
                sh.location_id = SYS_JITA as i64;
                sh.flag = InventoryFlag::None;
            }
            snap.items.insert(fid, make_item(fid, 72_982, cid, sid, InventoryFlag::Cargo, 1, false));
        }
        snap.items.insert(turret_id, make_item(turret_id, 10_678, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(ammo_id, make_item(ammo_id, 222, PILOT_A, ship_a, InventoryFlag::HiSlot0, 10, false));
    });
    sess_a.solar_system_id = Some(SYS_JITA);
    sess_a.station_id = None;
    sess_a.ship_id = Some(ship_a);
    sess_b.solar_system_id = Some(SYS_JITA);
    sess_b.station_id = None;
    sess_b.ship_id = Some(ship_b);
    ctx.invalidate_ship_fitting(ship_a);
    ctx.invalidate_ship_fitting(ship_b);

    let jita_h = ctx.systems.get_or_spawn(SYS_JITA);
    let _ = jita_h.remove_ball(ship_a).await;
    let _ = jita_h.remove_ball(ship_b).await;
    for &(cid, sid, pos) in &[
        (PILOT_A, ship_a, DVec3::new(0.0, 2.0e9, 0.0)),
        (PILOT_B, ship_b, DVec3::new(1_000.0, 2.0e9, 0.0)),
    ] {
        let mut cs = ctx.build_ship_combat_state(cid, sid, 621);
        cs.is_cloaked = false;
        cs.gate_cloak_remaining_ticks = 0;
        cs.undock_invulnerable_remaining_ticks = 0;
        let _ = jita_h
            .spawn_ball_with_meta(
                Ball {
                    id: sid,
                    type_id: 621,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_200_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
                ctx.build_ship_slim_meta(cid, 621),
            )
            .await;
    }

    let q_a = rpc(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(fil_a), PyRep::Integer(72_982)])
        .await
        .expect("PILOT_A JoinPVPQueue");
    assert_eq!(q_a, PyRep::Integer(30));
    let q_b = rpc(&ctx, &mut sess_b, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(fil_b), PyRep::Integer(72_982)])
        .await
        .expect("PILOT_B JoinPVPQueue");
    assert_eq!(q_b, PyRep::Integer(5));
    ctx.sync_session(&mut sess_a);
    ctx.sync_session(&mut sess_b);
    assert_eq!(sess_a.solar_system_id, Some(32_000_001));
    assert_eq!(sess_b.solar_system_id, Some(32_000_001));

    let arena_h = ctx.systems.get_or_spawn(32_000_001);
    let (_, ball_b) = arena_h.get_ball_snapshot(ship_b).await.expect("snap").expect("ship_b in arena");
    let _ = arena_h.set_ball_position(ship_a, ball_b.position + DVec3::new(-1_500.0, 0.0, 0.0)).await;
    let mut cs_a = arena_h.get_combat_state(ship_a).await.expect("cs_a").expect("ship_a combat");
    cs_a.is_cloaked = false;
    cs_a.gate_cloak_remaining_ticks = 0;
    cs_a.undock_invulnerable_remaining_ticks = 0;
    let _ = arena_h.set_combat_state(ship_a, cs_a).await;
    let mut cs_b = arena_h.get_combat_state(ship_b).await.expect("cs_b").expect("ship_b combat");
    cs_b.is_cloaked = false;
    cs_b.gate_cloak_remaining_ticks = 0;
    cs_b.undock_invulnerable_remaining_ticks = 0;
    (cs_b.shield_hp, cs_b.armor_hp, cs_b.structure_hp) = (0.0, 0.0, 1.0);
    let _ = arena_h.set_combat_state(ship_b, cs_b).await;
    let _ = arena_h
        .add_target_lock(
            ship_a,
            TargetLock {
                target_id: ship_b,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    ctx.set_invention_roll_override(Some(0.50));

    let fire_res = rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(turret_id), PyRep::Integer(ship_b)])
        .await
        .expect("Activate turret in Proving Grounds arena");
    assert_eq!(fire_res.dict_get("destroyed").and_then(PyRep::as_bool), Some(true));

    let pvp_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let pvp_stats_args = find_notif(&pvp_notifs_a, "OnPVPFilamentsCharacterStatistics");
    assert_eq!(pvp_stats_args.len(), 1);
    let PyRep::Dict(stat_entries) = &pvp_stats_args[0] else {
        panic!("expected PyRep::Dict for OnPVPFilamentsCharacterStatistics, got {:?}", pvp_stats_args[0]);
    };
    let stat_lookup = |key: &str| -> Option<&PyRep> {
        stat_entries.iter().find_map(|(k, v)| match k {
            PyRep::String(s) if s == key => Some(v),
            _ => None,
        })
    };
    assert_eq!(stat_lookup("eventID"), Some(&PyRep::Integer(expected_event_id)));
    assert_eq!(stat_lookup("matchTypeID"), Some(&PyRep::Integer(expected_match_type_id)));
    assert_eq!(stat_lookup("scheduleID"), Some(&PyRep::Integer(expected_schedule_id)));
    let Some(PyRep::Dict(inner_stats)) = stat_lookup("statistics") else {
        panic!("expected PyRep::Dict for statistics field, got {:?}", stat_lookup("statistics"));
    };
    let inner_lookup = |key: &str| -> Option<&PyRep> {
        inner_stats.iter().find_map(|(k, v)| match k {
            PyRep::String(s) if s == key => Some(v),
            _ => None,
        })
    };
    assert_eq!(inner_lookup("character_id"), Some(&PyRep::Integer(PILOT_A)));
    assert_eq!(inner_lookup("rank"), Some(&PyRep::Integer(1)));
    assert_eq!(inner_lookup("wins"), Some(&PyRep::Integer(1)));
    assert_eq!(inner_lookup("losses"), Some(&PyRep::Integer(0)));
    assert_eq!(inner_lookup("draws"), Some(&PyRep::Integer(0)));

    let pvp_lb_args = find_notif(&pvp_notifs_a, "OnPVPFilamentsLeaderboard");
    assert_eq!(pvp_lb_args.len(), 1);
    let PyRep::Dict(lb_entries) = &pvp_lb_args[0] else {
        panic!("expected PyRep::Dict for OnPVPFilamentsLeaderboard, got {:?}", pvp_lb_args[0]);
    };
    let lb_lookup = |key: &str| -> Option<&PyRep> {
        lb_entries.iter().find_map(|(k, v)| match k {
            PyRep::String(s) if s == key => Some(v),
            _ => None,
        })
    };
    assert_eq!(lb_lookup("eventID"), Some(&PyRep::Integer(expected_event_id)));
    assert_eq!(lb_lookup("matchTypeID"), Some(&PyRep::Integer(expected_match_type_id)));
    assert_eq!(lb_lookup("scheduleID"), Some(&PyRep::Integer(expected_schedule_id)));
    let Some(PyRep::List(rows)) = lb_lookup("entries") else {
        panic!("expected PyRep::List for entries field, got {:?}", lb_lookup("entries"));
    };
    assert_eq!(rows.len(), 2);
    let PyRep::Dict(row0) = &rows[0] else { panic!("expected PyRep::Dict for leaderboard row 0") };
    let row0_char = row0.iter().find_map(|(k, v)| match k {
        PyRep::String(s) if s == "character_id" => Some(v),
        _ => None,
    });
    let row0_rank = row0.iter().find_map(|(k, v)| match k {
        PyRep::String(s) if s == "rank" => Some(v),
        _ => None,
    });
    assert_eq!(row0_char, Some(&PyRep::Integer(PILOT_A)));
    assert_eq!(row0_rank, Some(&PyRep::Integer(1)));

    let pvp_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    let pvp_stats_b = find_notif(&pvp_notifs_b, "OnPVPFilamentsCharacterStatistics");
    assert_eq!(pvp_stats_b.len(), 1);
    let PyRep::Dict(stat_entries_b) = &pvp_stats_b[0] else {
        panic!("expected PyRep::Dict for PILOT_B OnPVPFilamentsCharacterStatistics");
    };
    let inner_b = stat_entries_b.iter().find_map(|(k, v)| match k {
        PyRep::String(s) if s == "statistics" => Some(v),
        _ => None,
    });
    let Some(PyRep::Dict(inner_stats_b)) = inner_b else {
        panic!("expected PyRep::Dict for PILOT_B statistics");
    };
    let b_char = inner_stats_b.iter().find_map(|(k, v)| match k {
        PyRep::String(s) if s == "character_id" => Some(v),
        _ => None,
    });
    let b_losses = inner_stats_b.iter().find_map(|(k, v)| match k {
        PyRep::String(s) if s == "losses" => Some(v),
        _ => None,
    });
    assert_eq!(b_char, Some(&PyRep::Integer(PILOT_B)));
    assert_eq!(b_losses, Some(&PyRep::Integer(1)));
    let pvp_lb_b = find_notif(&pvp_notifs_b, "OnPVPFilamentsLeaderboard");
    assert_eq!(pvp_lb_b.len(), 1);
    assert!(matches!(&pvp_lb_b[0], PyRep::Dict(_)));

    // 12. OnTournamentPerformBan (6-arg) via tourneyMgr.CreateMatch + StartAutobanning
    sess_a.role = eve_machonet::service_roles::ROLE_GMH;
    rpc(
        &ctx,
        &mut sess_a,
        "tourneyMgr",
        "CreateMatch",
        &[
            PyRep::Integer(1),
            PyRep::Tuple(vec![
                PyRep::Integer(1001),
                PyRep::Integer(98_000_001),
                PyRep::Integer(98_000_002),
                PyRep::Integer(PILOT_A),
                PyRep::Integer(PILOT_B),
            ]),
            PyRep::Integer(100),
            PyRep::Integer(600),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("tourneyMgr.CreateMatch");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(&ctx, &mut sess_a, "tourneyMgr", "StartAutobanning", &[PyRep::Integer(1001), PyRep::Integer(2)])
        .await
        .expect("tourneyMgr.StartAutobanning");
    let ban_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
    let ban_args = find_notif(&ban_notifs, "OnTournamentPerformBan");
    assert_eq!(ban_args.len(), 6);
    assert_eq!(ban_args[0], PyRep::Integer(1001));
    assert_eq!(ban_args[1], PyRep::Integer(2));
    assert!(matches!(&ban_args[2], PyRep::List(l) if !l.is_empty()));
    assert!(ban_args[3].as_i64().unwrap_or(0) > 0);
    assert_eq!(ban_args[4], PyRep::Integer(1));
    assert_eq!(ban_args[5], PyRep::Integer(1));
}

}
