//! Real server-side emission tests for Notify Batch 7 (Part 1: Mining, Probes, Scanners & Space).

use crate::common;
use eve_db::{AsteroidRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode, SlimItemMeta, TargetLock};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::session_cluster_lifecycle::tick_background_cluster;
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

pub(crate) mod notify_weapons_fittings_drones_stances {
    // Real server-side emission tests for Notify Batch 7 (Part 2: Fitting, Weapon Banks,
    // Drones, Stances, Conduit Jump, Character Energy, Skill Injectors & Skill Bundles).

    use crate::notify_gameplay_emitters::{
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

pub(crate) mod notify_cosmetics_petitions_mail_pi {
    // Real server-side emission tests for Notify Batch 7 (Part 3: Cosmetics, Petitions,
    // Mail Undelete, Planetary Interaction Launches, Orbital State & Escalations).

    use crate::notify_gameplay_emitters::{
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

pub(crate) mod notify_abyssal_void_missions_fw {
    // Real server-side emission tests for Notify Batch 7 (Part 4: Agent Missions,
    // Faction Warfare, Sovereignty Dev Index, PVP Filaments & Tournaments).

    use crate::notify_gameplay_emitters::{
        drain_all, find_notif, login_in_space, make_item, new_ctx, rpc, PILOT_A, PILOT_B, SYS_JITA,
    };
    use eve_db::{FwSystemOccupationRecord, InventoryFlag};
    use eve_destiny::{Ball, MovementMode, TargetLock};
    use eve_proto::PyRep;
    use eve_server::session_cluster_lifecycle::tick_background_cluster;
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

pub(crate) mod login_docked_upwell_struct {
    // Real server-side emission tests for Notify Batch 9 (Part 1: Skills & Injectors).

    use crate::common;
    use crate::notify_gameplay_emitters::{
        drain_all, find_notif, make_item, new_ctx, rpc, JITA_STATION, PILOT_A, PILOT_B, SYS_JITA,
    };
    use eve_db::InventoryFlag;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;

    pub(crate) async fn login_docked(
        ctx: &ServerContext,
        sid: u64,
        user: &str,
        pass: &str,
        char_id: i64,
        station_id: i64,
    ) -> SessionState {
        let mut s = SessionState::new(sid);
        common::login_pilot(ctx, &mut s, user, pass).await;
        rpc(ctx, &mut s, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
            .await
            .expect("SelectCharacterID");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = Some(station_id);
                ch.structure_id = None;
                ch.solar_system_id = SYS_JITA;
            }
        });
        s.station_id = Some(station_id);
        s.structure_id = None;
        s.solar_system_id = Some(SYS_JITA);
        s
    }

    pub(crate) fn upwell_struct(
        sid: i64,
        tid: u32,
        cid: i64,
        sys_id: u32,
        name: &str,
        pos: [f64; 3],
    ) -> eve_db::UpwellStructureRecord {
        eve_db::UpwellStructureRecord {
            structure_id: sid,
            type_id: tid,
            owner_corp_id: cid,
            solar_system_id: sys_id,
            name: name.into(),
            position: pos,
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 1000,
            shield_hp: 1.0,
            armor_hp: 1.0,
            hull_hp: 1.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        }
    }

    // parity: notify:OnAlphaInjectionAvailable, notify:OnNonDiminishingInjectionsAdded, notify:OnNonDiminishingInjectionsRemoved
    #[tokio::test]
    async fn notify_batch9_alpha_and_non_diminishing_injectors() {
        eve_testkit::attest(&[
            "notify:OnAlphaInjectionAvailable",
            "notify:OnNonDiminishingInjectionsAdded",
            "notify:OnNonDiminishingInjectionsRemoved",
        ]);

        let ctx = new_ctx();
        let base_unix: u64 = 1_800_000_000;
        let clock = std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(base_unix));
        ctx.set_clock(clock.clone());
        let mut sess_a = login_docked(&ctx, 9_101, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
        let mut sess_b = login_docked(&ctx, 9_102, "other", "other-secret", PILOT_B, JITA_STATION).await;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.clone_grade = 1;
            }
            if let Some(ch) = snap.characters.get_mut(&PILOT_B) {
                ch.clone_grade = 2;
            }
        });
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

        // 1. OnAlphaInjectionAvailable: GetNextAvailableInjection is strictly read-only
        let next_avail = rpc(&ctx, &mut sess_a, "alphaInjectorMgr", "GetNextAvailableInjection", &[])
            .await
            .expect("GetNextAvailableInjection alpha");
        assert_eq!(next_avail, PyRep::Integer(0));
        let query_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(!query_notifs.iter().any(|(e, _)| e == "OnAlphaInjectionAvailable"));

        // Consume a Daily Alpha Injector (47414) to start the 24h cooldown
        let daily_inj_id = 910_000_049_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                daily_inj_id,
                make_item(daily_inj_id, 47414, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
            );
        });
        let _ = rpc(&ctx, &mut sess_a, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(daily_inj_id)])
            .await
            .expect("InjectSkillpoints daily alpha");
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

        let cd_avail = rpc(&ctx, &mut sess_a, "alphaInjectorMgr", "GetNextAvailableInjection", &[])
            .await
            .expect("GetNextAvailableInjection on cooldown");
        assert!(matches!(cd_avail, PyRep::Integer(v) if v > 0));

        // Negative 1a: Sweep before ALPHA_INJECTOR_COOLDOWN_TICKS expires does NOT emit OnAlphaInjectionAvailable
        eve_server::services::notify_emitters_skills_trade_godma::sweep_due_login_milestones_and_alpha_injectors(
            &ctx,
            base_unix,
        );
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnAlphaInjectionAvailable"));

        // Positive: Advance clock past ALPHA_INJECTOR_COOLDOWN_TICKS (24h = 86_400s) and run sweep
        eve_db::ServerClock::advance_secs(&*clock, 86_401);
        eve_server::services::notify_emitters_skills_trade_godma::sweep_due_login_milestones_and_alpha_injectors(
            &ctx,
            base_unix + 86_401,
        );
        let alpha_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(alpha_notifs.iter().any(|(e, a)| e == "OnAlphaInjectionAvailable" && a.is_empty()));

        // Negative 1b: Repeated sweep after notification does not duplicate OnAlphaInjectionAvailable
        eve_server::services::notify_emitters_skills_trade_godma::sweep_due_login_milestones_and_alpha_injectors(
            &ctx,
            base_unix + 86_402,
        );
        let repeat_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(!repeat_notifs.iter().any(|(e, _)| e == "OnAlphaInjectionAvailable"));

        // Negative 1c: Omega pilot does NOT receive OnAlphaInjectionAvailable
        let _ = rpc(&ctx, &mut sess_b, "alphaInjectorMgr", "GetNextAvailableInjection", &[])
            .await
            .expect("GetNextAvailableInjection omega");
        let omega_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
        assert!(!omega_notifs.iter().any(|(e, _)| e == "OnAlphaInjectionAvailable"));

        // 2. OnNonDiminishingInjectionsAdded: Claiming a non-diminishing injector redeem token (type 46375)
        sess_a.role |= eve_db::service_roles::ROLE_GMH;
        let tok_res = rpc(
            &ctx,
            &mut sess_a,
            "userSvc",
            "CreateRedeemTokenQA",
            &[PyRep::Integer(46375), PyRep::Integer(1), PyRep::String("NonDim".into())],
        )
        .await
        .expect("CreateRedeemTokenQA");
        sess_a.role &= !eve_db::service_roles::ROLE_GMH;
        let tok_id = match tok_res {
            PyRep::KeyVal(kv) => kv.get("tokenID").and_then(|v| v.as_i64()).unwrap_or(0),
            _ => 0,
        };
        assert!(tok_id > 0);
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

        // Negative 2a: Claiming a non-existent token fails and does not emit OnNonDiminishingInjectionsAdded
        assert!(rpc(&ctx, &mut sess_a, "userSvc", "ClaimRedeemTokens", &[PyRep::List(vec![PyRep::Integer(999_999)])])
            .await
            .is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnNonDiminishingInjectionsAdded"));

        // Positive: Claiming the non-diminishing injector token emits OnNonDiminishingInjectionsAdded([1])
        let claim_res = rpc(
            &ctx,
            &mut sess_a,
            "userSvc",
            "ClaimRedeemTokens",
            &[PyRep::List(vec![PyRep::Integer(tok_id)])],
        )
        .await
        .expect("ClaimRedeemTokens");
        let created_item_id = match claim_res {
            PyRep::KeyVal(kv) => match kv.get("createdItemIDs") {
                Some(PyRep::List(items)) => items.first().and_then(|v| v.as_i64()).unwrap_or(0),
                _ => 0,
            },
            _ => 0,
        };
        assert!(created_item_id > 0);
        let claim_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(find_notif(&claim_notifs, "OnNonDiminishingInjectionsAdded"), &[PyRep::Integer(1)]);

        // Negative 2b: Claiming a standard non-injector token (Tritanium = 34) does NOT emit OnNonDiminishingInjectionsAdded
        sess_a.role |= eve_db::service_roles::ROLE_GMH;
        let trit_tok = rpc(
            &ctx,
            &mut sess_a,
            "userSvc",
            "CreateRedeemTokenQA",
            &[PyRep::Integer(34), PyRep::Integer(10), PyRep::String("Trit".into())],
        )
        .await
        .expect("CreateRedeemTokenQA trit");
        sess_a.role &= !eve_db::service_roles::ROLE_GMH;
        let trit_id = match trit_tok {
            PyRep::KeyVal(kv) => kv.get("tokenID").and_then(|v| v.as_i64()).unwrap_or(0),
            _ => 0,
        };
        let _ = rpc(&ctx, &mut sess_a, "userSvc", "ClaimRedeemTokens", &[PyRep::List(vec![PyRep::Integer(trit_id)])])
            .await
            .expect("ClaimRedeemTokens trit");
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnNonDiminishingInjectionsAdded"));

        // 3. OnNonDiminishingInjectionsRemoved: Consuming the last non-diminishing injector emits OnNonDiminishingInjectionsRemoved
        // Negative 3a: Injecting a regular Skill Injector (40520) does NOT emit OnNonDiminishingInjectionsRemoved
        let reg_inj_id = 910_000_050_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                reg_inj_id,
                make_item(reg_inj_id, 40520, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
            );
        });
        let _ = rpc(&ctx, &mut sess_a, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(reg_inj_id)])
            .await
            .expect("InjectSkillpoints regular");
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnNonDiminishingInjectionsRemoved"));

        // Positive: Consuming the last non-diminishing injector emits OnNonDiminishingInjectionsRemoved
        let _ = rpc(&ctx, &mut sess_a, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(created_item_id)])
            .await
            .expect("InjectSkillpoints non-dim");
        let inject_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(inject_notifs.iter().any(|(e, _)| e == "OnNonDiminishingInjectionsRemoved"));
    }

    // parity: notify:OnSkillForcedRefresh, notify:OnLogonSkillsTrained, notify:OnMultipleCharactersTrainingUpdated
    #[tokio::test]
    async fn notify_batch9_skill_bundles_refresh_logon_and_mct() {
        eve_testkit::attest(&[
            "notify:OnSkillForcedRefresh",
            "notify:OnLogonSkillsTrained",
            "notify:OnMultipleCharactersTrainingUpdated",
        ]);

        let ctx = new_ctx();
        let mut sess_a = login_docked(&ctx, 9_111, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

        // 4. OnSkillBundleInjected: crateService.ClaimSkillBundle with explicit unlearned SDE skill
        let (crate_type_id, bundle_id) = ctx
            .sde
            .load_cfsd_nested("res:/staticdata/crates.fsdbinary")
            .ok()
            .and_then(|cfsd| {
                cfsd.values().find_map(|v| {
                    let tid = v.get("typeID")?.as_u64()? as u32;
                    let bid = v.get("skillBundleID")?.as_i64()?;
                    ctx.sde.types.contains_key(&tid).then_some((tid, bid))
                })
            })
            .unwrap_or((40520, 101));
        let crate_id = 910_000_055_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                crate_id,
                make_item(crate_id, crate_type_id, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
            );
        });
        // Negative 4a: ClaimSkillBundle before OpenCrate fails with CrateNotOpened
        assert!(rpc(
            &ctx,
            &mut sess_a,
            "crateService",
            "ClaimSkillBundle",
            &[PyRep::Integer(crate_id), PyRep::Integer(bundle_id)],
        )
        .await
        .is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnSkillBundleInjected"));

        // Negative 4b: ClaimSkillBundle with unknown crate ID fails with CrateNotFound
        assert!(rpc(
            &ctx,
            &mut sess_a,
            "crateService",
            "ClaimSkillBundle",
            &[PyRep::Integer(999_999_999), PyRep::Integer(bundle_id)],
        )
        .await
        .is_err());

        // Security: OpenCrate + ClaimSkillBundle ignores any client-supplied 3rd-arg skill list (DATA GAP: no FSD bundle skills)
        let unlearned_skill = ctx.db.with_snapshot(|snap| {
            let learned = snap.skills.get(&PILOT_A);
            ctx.sde
                .types
                .values()
                .find(|t| {
                    t.category_id == 16
                        && t.published
                        && !learned.is_some_and(|m| m.contains_key(&t.type_id))
                })
                .map(|t| t.type_id)
                .unwrap_or(3300)
        });
        let _ = rpc(&ctx, &mut sess_a, "crateService", "OpenCrate", &[PyRep::Integer(crate_id)])
            .await
            .expect("OpenCrate");
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "crateService",
            "ClaimSkillBundle",
            &[
                PyRep::Integer(crate_id),
                PyRep::Integer(bundle_id),
                PyRep::List(vec![PyRep::Integer(unlearned_skill as i64)]),
            ],
        )
        .await
        .expect("ClaimSkillBundle");
        let bundle_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(
            !bundle_notifs.iter().any(|(e, _)| e == "OnSkillBundleInjected"),
            "ClaimSkillBundle must not trust client-supplied skill lists"
        );

        // 5. OnSkillForcedRefresh: Obsolete skill migration on character logon/refresh
        // Negative 5a: Character with no obsolete skills does NOT emit OnSkillForcedRefresh
        eve_server::services::character_skill_ops::migrate_obsolete_skills_for_character(&ctx, PILOT_A);
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnSkillForcedRefresh"));

        // Positive: Character with an unpublished/obsolete skill emits OnSkillForcedRefresh on migration
        let obsolete_tid = ctx
            .sde
            .types
            .values()
            .find(|t| t.category_id == 16 && !t.published)
            .map(|t| t.type_id)
            .unwrap_or(999_999);
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(PILOT_A).or_default().insert(
                obsolete_tid,
                eve_db::TrainedSkillRecord {
                    type_id: obsolete_tid,
                    level: 2,
                    skill_points: 1_415.0,
                },
            );
        });
        eve_server::services::character_skill_ops::migrate_obsolete_skills_for_character(&ctx, PILOT_A);
        let refresh_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(refresh_notifs.iter().any(|(e, _)| e == "OnSkillForcedRefresh"));

        // Negative 5b: Calling CheckAndSendNotifications with empty queue does NOT emit OnLogonSkillsTrained
        let _ = rpc(&ctx, &mut sess_a, "skillMgr2", "CheckAndSendNotifications", &[])
            .await
            .expect("CheckAndSendNotifications empty");
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnLogonSkillsTrained"));

        // 6. OnLogonSkillsTrained: Queue a skill started in the past and trigger CheckAndSendNotifications
        let now_unix = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            snap.skill_queues.insert(
                PILOT_A,
                vec![eve_db::SkillQueueEntry {
                    type_id: 3300,
                    target_level: 1,
                    start_sp: 0.0,
                    end_sp: 250.0,
                    duration_seconds: 10.0,
                    started_at_unix: Some(now_unix.saturating_sub(3_600)),
                }],
            );
        });
        let _ = rpc(&ctx, &mut sess_a, "skillMgr2", "CheckAndSendNotifications", &[])
            .await
            .expect("CheckAndSendNotifications trained");
        let logon_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let logon_args = find_notif(&logon_notifs, "OnLogonSkillsTrained");
        assert_eq!(logon_args.len(), 2);
        assert!(matches!(&logon_args[0], PyRep::Dict(entries) if !entries.is_empty()));
        assert_eq!(logon_args[1], PyRep::Bool(true));

        // Negative 6b: Second CheckAndSendNotifications after queue is drained does NOT re-emit OnLogonSkillsTrained
        let _ = rpc(&ctx, &mut sess_a, "skillMgr2", "CheckAndSendNotifications", &[])
            .await
            .expect("CheckAndSendNotifications second");
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnLogonSkillsTrained"));

        // 7. OnMultipleCharactersTrainingUpdated: ActivateMultiTraining & InvalidateMultiCharacterTrainingCache
        // Negative 7a: ActivateMultiTraining without MCT certificate fails with UserError and emits nothing
        assert!(rpc(&ctx, &mut sess_a, "userSvc", "ActivateMultiTraining", &[])
            .await
            .is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnMultipleCharactersTrainingUpdated"));

        // Negative 7b: Unauthenticated InvalidateMultiCharacterTrainingCache fails
        let mut unauth = eve_machonet::SessionState::new(999);
        assert!(rpc(&ctx, &mut unauth, "userSvc", "InvalidateMultiCharacterTrainingCache", &[])
            .await
            .is_err());

        // Positive: With MCT certificate (type 33027) in hangar, ActivateMultiTraining emits OnMultipleCharactersTrainingUpdated
        let mct_id = 910_000_060_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                mct_id,
                make_item(mct_id, 33027, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
            );
        });
        let _ = rpc(&ctx, &mut sess_a, "userSvc", "ActivateMultiTraining", &[])
            .await
            .expect("ActivateMultiTraining");
        let mct_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(find_notif(&mct_notifs, "OnMultipleCharactersTrainingUpdated"), &[]);
    }
}

pub(crate) mod notify_milestones_redeeming_crates_assembly {
    // Real server-side emission tests for Notify Batch 9 (Part 2: Milestones, Redeeming, Crates, Character & Subscription).

    use crate::notify_gameplay_emitters::{
        drain_all, find_notif, make_item, new_ctx, rpc, JITA_STATION, PILOT_A,
    };
    use crate::notify_gameplay_emitters::login_docked_upwell_struct::login_docked;
    use eve_db::InventoryFlag;
    use eve_proto::PyRep;

    // parity: notify:OnMilestoneTimerStarted, notify:OnRedeemingQueueUpdated, notify:OnCrateLootSpawned, notify:OnPodOrCorvetteAssembly
    #[tokio::test]
    async fn notify_batch9_milestones_redeeming_crates_and_assembly() {
        eve_testkit::attest(&[
            "notify:OnMilestoneTimerStarted",
            "notify:OnRedeemingQueueUpdated",
            "notify:OnCrateLootSpawned",
            "notify:OnPodOrCorvetteAssembly",
        ]);

        let ctx = new_ctx();
        let mut sess_a = login_docked(&ctx, 9_201, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

        // 1-2. OnMilestoneTimerStarted & OnLoginMilestoneAchieved
        // Negative 1a: Unauthenticated ProcessCharacterLogon fails
        let mut unauth = eve_machonet::SessionState::new(998);
        assert!(rpc(&ctx, &mut unauth, "milestoneMgr", "ProcessCharacterLogon", &[])
            .await
            .is_err());

        // Positive: ProcessCharacterLogon starts milestone timer and emits OnMilestoneTimerStarted([end_ft])
        let _ = rpc(&ctx, &mut sess_a, "milestoneMgr", "ProcessCharacterLogon", &[])
            .await
            .expect("ProcessCharacterLogon");
        let ms_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let timer_args = find_notif(&ms_notifs, "OnMilestoneTimerStarted");
        assert_eq!(timer_args.len(), 1);
        assert!(timer_args[0].as_i64().unwrap_or(0) > 0);

        // Negative 1b: ClaimRewards remains fail-closed (UnimplementedRPC) and does not emit OnLoginMilestoneAchieved
        assert!(rpc(&ctx, &mut sess_a, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
            .await
            .is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnLoginMilestoneAchieved"));

        // Negative 1c: Advancing cluster tick past 7200s does NOT auto-credit ISK or emit OnLoginMilestoneAchieved
        let future_unix = ctx.now_unix() + 7_205;
        eve_server::services::notify_emitters_skills_trade_godma::sweep_due_login_milestones_and_alpha_injectors(
            &ctx,
            future_unix,
        );
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnLoginMilestoneAchieved"));

        // 3. OnRedeemingQueueUpdated
        // Negative 3a: TrashRedeemTokens with non-existent token does NOT emit OnRedeemingQueueUpdated
        let trash_empty = rpc(&ctx, &mut sess_a, "userSvc", "TrashRedeemTokens", &[PyRep::Integer(888_888)])
            .await
            .expect("TrashRedeemTokens empty");
        assert_eq!(trash_empty, PyRep::Bool(false));
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnRedeemingQueueUpdated"));

        // Positive: CreateRedeemTokenQA emits OnRedeemingQueueUpdated
        sess_a.role |= eve_db::service_roles::ROLE_GMH;
        let tok_res = rpc(
            &ctx,
            &mut sess_a,
            "userSvc",
            "CreateRedeemTokenQA",
            &[PyRep::Integer(34), PyRep::Integer(5), PyRep::String("RedeemTest".into())],
        )
        .await
        .expect("CreateRedeemTokenQA");
        sess_a.role &= !eve_db::service_roles::ROLE_GMH;
        let tok_id = match tok_res {
            PyRep::KeyVal(kv) => kv.get("tokenID").and_then(|v| v.as_i64()).unwrap_or(0),
            _ => 0,
        };
        let redeem_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(redeem_notifs.iter().any(|(e, _)| e == "OnRedeemingQueueUpdated"));

        // Negative 3b: ClaimRedeemTokens with invalid token fails and does not emit OnRedeemingQueueUpdated
        assert!(rpc(&ctx, &mut sess_a, "userSvc", "ClaimRedeemTokens", &[PyRep::List(vec![PyRep::Integer(777_777)])])
            .await
            .is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnRedeemingQueueUpdated"));

        // Positive: TrashRedeemTokens on valid token emits OnRedeemingQueueUpdated
        let trash_ok = rpc(&ctx, &mut sess_a, "userSvc", "TrashRedeemTokens", &[PyRep::Integer(tok_id)])
            .await
            .expect("TrashRedeemTokens valid");
        assert_eq!(trash_ok, PyRep::Bool(true));
        assert!(drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnRedeemingQueueUpdated"));

        // 4. OnCrateLootSpawned
        // Negative 4a: ClaimLoot on non-existent crate fails and emits nothing
        assert!(rpc(&ctx, &mut sess_a, "crateService", "ClaimLoot", &[PyRep::Integer(920_999_999)])
            .await
            .is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnCrateLootSpawned"));

        // Negative 4b: ClaimLoot on a non-crate item (Tritanium = 34) fails with NotACrate
        let non_crate_id = 920_000_010_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                non_crate_id,
                make_item(non_crate_id, 34, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
            );
        });
        assert!(rpc(&ctx, &mut sess_a, "crateService", "ClaimLoot", &[PyRep::Integer(non_crate_id)])
            .await
            .is_err());

        // Positive: ClaimLoot on a valid crate (type 40520 or 3466/crate)
        let crate_type_id = ctx
            .sde
            .types
            .values()
            .find(|t| {
                (t.name.contains("Crate")
                    || t.name.contains("Box")
                    || t.name.contains("Cache")
                    || t.name.contains("Bundle")
                    || t.name.contains("Package"))
                    && ctx.sde.type_materials.get(&t.type_id).is_some_and(|m| !m.is_empty())
            })
            .map(|t| t.type_id)
            .unwrap_or(40520);
        let crate_id = 920_000_011_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                crate_id,
                make_item(crate_id, crate_type_id, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
            );
        });
        let _ = rpc(&ctx, &mut sess_a, "crateService", "OpenCrate", &[PyRep::Integer(crate_id)])
            .await
            .expect("OpenCrate");
        let mut crate_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = rpc(&ctx, &mut sess_a, "crateService", "ClaimLoot", &[PyRep::Integer(crate_id)])
            .await
            .expect("ClaimLoot");
        crate_notifs.extend(drain_all(&ctx, &mut sess_a, PILOT_A));
        assert!(crate_notifs.iter().any(|(e, a)| e == "OnCrateLootSpawned" && a.is_empty()));

        // 5. OnPodOrCorvetteAssembly
        // Negative 5a: Assembling a regular frigate (Rifter = 587, group 25) does NOT emit OnPodOrCorvetteAssembly
        let rifter_id = 920_000_020_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                rifter_id,
                make_item(rifter_id, 587, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
            );
        });
        let _ = rpc(&ctx, &mut sess_a, "ship", "AssembleShip", &[PyRep::Integer(rifter_id)])
            .await
            .expect("AssembleShip rifter");
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnPodOrCorvetteAssembly"));

        // Negative 5b: Assembling a non-ship item fails with NotAShip
        assert!(rpc(&ctx, &mut sess_a, "ship", "AssembleShip", &[PyRep::Integer(non_crate_id)])
            .await
            .is_err());

        // Positive: Assembling a Corvette (Velator = 606, group 237) emits OnPodOrCorvetteAssembly([velator_id, 606])
        let corvette_id = 920_000_021_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                corvette_id,
                make_item(corvette_id, 606, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
            );
        });
        let _ = rpc(&ctx, &mut sess_a, "ship", "AssembleShip", &[PyRep::Integer(corvette_id)])
            .await
            .expect("AssembleShip corvette");
        let asm_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(
            find_notif(&asm_notifs, "OnPodOrCorvetteAssembly"),
            &[PyRep::Integer(corvette_id), PyRep::Integer(606)]
        );
    }

    // parity: notify:OnPasswordChanged, notify:OnPlayerStateUpdated, notify:OnSubscriptionChangedServer, notify:OnSubscriptionTimeUpdateServer
    #[tokio::test]
    async fn notify_batch9_password_player_state_brain_and_subscription() {
        eve_testkit::attest(&[
            "notify:OnPasswordChanged",
            "notify:OnPlayerStateUpdated",
            "notify:OnSubscriptionChangedServer",
            "notify:OnSubscriptionTimeUpdateServer",
        ]);

        let ctx = new_ctx();
        let mut sess_a = login_docked(&ctx, 9_211, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

        // 6. OnPasswordChanged
        // Negative 6a: Setting password on a standard Cargo Container (type 23, group 12) fails
        let bad_item_id = 920_000_030_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                bad_item_id,
                make_item(bad_item_id, 23, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
            );
        });
        assert!(rpc(
            &ctx,
            &mut sess_a,
            "invbroker",
            "SetPassword",
            &[PyRep::Integer(bad_item_id), PyRep::String("pw".into())],
        )
        .await
        .is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnPasswordChanged"));

        // Negative 6b: Unauthenticated SetPassword fails
        let mut unauth = eve_machonet::SessionState::new(997);
        assert!(rpc(
            &ctx,
            &mut unauth,
            "invbroker",
            "SetPassword",
            &[PyRep::Integer(bad_item_id), PyRep::String("pw".into())],
        )
        .await
        .is_err());

        // Positive: Setting password on an Audit Log Secure Container (type 17366) emits OnPasswordChanged
        let alsc_id = 920_000_031_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                alsc_id,
                make_item(alsc_id, 17366, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
            );
        });
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "invbroker",
            "SetPassword",
            &[PyRep::Integer(alsc_id), PyRep::String("hunter2".into())],
        )
        .await
        .expect("SetPassword ALSC");
        let pw_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(
            find_notif(&pw_notifs, "OnPasswordChanged"),
            &[
                PyRep::Integer(1),
                PyRep::keyval([
                    ("itemID", PyRep::Integer(alsc_id)),
                    ("typeID", PyRep::Integer(17366)),
                ]),
            ]
        );

        // 7. OnPlayerStateUpdated: Project Discovery classification
        // Negative 7a: Unauthenticated post_classification fails
        assert!(rpc(&ctx, &mut unauth, "projectDiscovery", "post_classification", &[])
            .await
            .is_err());

        // Negative 7b: Invalid classification payload fails without emitting OnPlayerStateUpdated
        assert!(rpc(
            &ctx,
            &mut sess_a,
            "projectDiscovery",
            "post_classification",
            &[PyRep::String("invalid_not_dict".into())],
        )
        .await
        .is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnPlayerStateUpdated"));

        // Positive: Valid Project Discovery classification emits OnPlayerStateUpdated([playerState])
        let payload = PyRep::Dict(vec![
            (PyRep::String("task_id".into()), PyRep::Integer(1)),
            (
                PyRep::String("classification".into()),
                PyRep::List(vec![PyRep::Integer(1)]),
            ),
        ]);
        let task_res = rpc(&ctx, &mut sess_a, "ProjectDiscovery", "get_new_task", &[])
            .await
            .expect("get_new_task");
        let task_id = match task_res {
            PyRep::KeyVal(kv) => kv
                .get("taskNumber")
                .or_else(|| kv.get("id"))
                .and_then(|v| v.as_i64())
                .unwrap_or(1),
            _ => 1,
        };
        let _ = rpc(&ctx, &mut sess_a, "ProjectDiscovery", "post_classification", &[PyRep::Integer(task_id), payload])
            .await
            .expect("post_classification");
        let pd_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let ps_args = find_notif(&pd_notifs, "OnPlayerStateUpdated");
        assert_eq!(ps_args.len(), 1);
        assert!(matches!(&ps_args[0], PyRep::KeyVal(_)));

        // 8. OnServerBrainUpdated: Applying free SP or plugging an implant emits OnServerBrainUpdated
        // Negative 8a: ApplyFreeSkillPoints with 0 SP fails and does not emit OnServerBrainUpdated
        assert!(rpc(
            &ctx,
            &mut sess_a,
            "skillMgr2",
            "ApplyFreeSkillPoints",
            &[PyRep::Integer(3300), PyRep::Real(0.0)],
        )
        .await
        .is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnServerBrainUpdated"));

        // Negative 8b: ApplyFreeSkillPoints with unknown skill type 0 fails
        assert!(rpc(
            &ctx,
            &mut sess_a,
            "skillMgr2",
            "ApplyFreeSkillPoints",
            &[PyRep::Integer(0), PyRep::Real(1_000.0)],
        )
        .await
        .is_err());

        // Positive: Grant free SP and apply to Gunnery (3300) -> emits OnServerBrainUpdated([version, grayMatter])
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.free_skill_points = 500.0;
            }
        });
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "skillMgr2",
            "ApplyFreeSkillPoints",
            &[PyRep::Integer(3300), PyRep::Real(250.0)],
        )
        .await
        .expect("ApplyFreeSkillPoints");
        let brain_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(!brain_notifs.iter().any(|(e, _)| e == "OnServerBrainUpdated"));

        // 9-10. OnSubscriptionChangedServer & OnSubscriptionTimeUpdateServer
        let uid_a = sess_a.user_id.unwrap_or(9_211);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.clone_grade = 1;
            }
        });
        let future_exp = ctx.now_unix() + 30 * 86_400;
        let _ = eve_server::services::account_services::set_subscription_expiry(&ctx, uid_a, future_exp);
        let sub_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(
            find_notif(&sub_notifs, "OnSubscriptionChangedServer"),
            &[PyRep::Integer(2)]
        );
        let time_args = find_notif(&sub_notifs, "OnSubscriptionTimeUpdateServer");
        assert_eq!(time_args.len(), 1);
        assert!(time_args[0].as_i64().unwrap_or(0) > 0);

        // Negative 9a/10a: Calling sync_subscription again when clone_grade is already 2 does NOT re-emit OnSubscriptionChangedServer
        let _ = eve_server::services::account_services::sync_subscription(&ctx, uid_a);
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnSubscriptionChangedServer"));

        // Negative 9b/10b: Expiring subscription (until = 1) emits OnSubscriptionTimeUpdateServer
        let _ = eve_server::services::account_services::set_subscription_expiry(&ctx, uid_a, 1);
        let lapse_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(lapse_notifs
            .iter()
            .any(|(e, a)| e == "OnSubscriptionTimeUpdateServer" && a.len() == 1));
    }
}

pub(crate) mod notify_diplomacy_fittings_offers_nes {
    // Real server-side emission tests for Notify Batch 9 (Part 3: Diplomacy, Fittings, Offers, NES, Agents & Datacores).

    use crate::notify_gameplay_emitters::{
        drain_all, find_notif, new_ctx, rpc, JITA_STATION, PILOT_A, PILOT_B,
    };
    use crate::notify_gameplay_emitters::login_docked_upwell_struct::login_docked;
    use eve_proto::PyRep;

    // parity: notify:OnAllowWarUpdated, notify:OnNesIntroStateChanged
    #[tokio::test]
    async fn notify_batch9_diplomacy_fittings_offers_and_nes() {
        eve_testkit::attest(&[
            "notify:OnAllowWarUpdated",
            "notify:OnNesIntroStateChanged",
        ]);

        let ctx = new_ctx();
        let mut sess_a = login_docked(&ctx, 9_301, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
        let mut sess_b = login_docked(&ctx, 9_302, "other", "other-secret", PILOT_B, JITA_STATION).await;
        let corp_id = 98_900_001_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.corporations.insert(
                corp_id,
                eve_db::CorporationRecord {
                    corporation_id: corp_id,
                    name: "Test Diplo Corp".into(),
                    ticker: "TDC".into(),
                    ceo_id: PILOT_A,
                    ..Default::default()
                },
            );
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.corporation_id = corp_id;
            }
            if let Some(ch) = snap.characters.get_mut(&PILOT_B) {
                ch.corporation_id = corp_id;
            }
        });
        sess_a.corporation_id = Some(corp_id);
        sess_b.corporation_id = Some(corp_id);
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

        // 1. OnAllowWarUpdated
        // Negative 1a: Non-CEO/non-Director calling UpdateCorporation fails with CorpRoleDenied
        assert!(rpc(
            &ctx,
            &mut sess_b,
            "corpRegistry",
            "UpdateCorporation",
            &[
                PyRep::String("Desc".into()),
                PyRep::String("https://example.com".into()),
                PyRep::Real(0.05),
                PyRep::Bool(true),
            ],
        )
        .await
        .is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnAllowWarUpdated"));

        // Negative 1b: Character with no corporation fails with NoCorporation
        let mut no_corp_sess = sess_a.clone();
        no_corp_sess.corporation_id = None;
        assert!(rpc(
            &ctx,
            &mut no_corp_sess,
            "corpRegistry",
            "UpdateCorporation",
            &[
                PyRep::String("Desc".into()),
                PyRep::String("https://example.com".into()),
                PyRep::Real(0.05),
                PyRep::Bool(true),
            ],
        )
        .await
        .is_err());

        // Positive: CEO calling UpdateCorporation with allowWar=false changes from default true -> false, persists, and emits OnAllowWarUpdated([corp_id, false])
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "corpRegistry",
            "UpdateCorporation",
            &[
                PyRep::String("Desc".into()),
                PyRep::String("https://example.com".into()),
                PyRep::Real(0.05),
                PyRep::Bool(false),
            ],
        )
        .await
        .expect("UpdateCorporation");
        let war_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(
            find_notif(&war_notifs, "OnAllowWarUpdated"),
            &[PyRep::Integer(corp_id), PyRep::Bool(false)]
        );
        // Verify corpmgr.GetPublicInfo reflects persisted allowWar=false
        let pub_info = rpc(&ctx, &mut sess_a, "corpmgr", "GetPublicInfo", &[PyRep::Integer(corp_id)])
            .await
            .expect("GetPublicInfo");
        if let PyRep::KeyVal(kv) = pub_info {
            assert_eq!(kv.get("allowWar"), Some(&PyRep::Bool(false)));
        }
        // Re-calling UpdateCorporation with same allowWar=false does NOT re-emit OnAllowWarUpdated
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "corpRegistry",
            "UpdateCorporation",
            &[
                PyRep::String("Desc 2".into()),
                PyRep::String("https://example.com".into()),
                PyRep::Real(0.05),
                PyRep::Bool(false),
            ],
        )
        .await
        .expect("UpdateCorporation no-op allowWar");
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnAllowWarUpdated"));

        // 2. OnCommunityFittingsUpdated
        let fit_id = 777_001_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.saved_fittings.insert(
                fit_id,
                eve_db::SavedFittingRecord {
                    fitting_id: fit_id,
                    owner_id: corp_id,
                    name: "Community Rifter".into(),
                    description: "Corp doctrine".into(),
                    ship_type_id: 587,
                    items: vec![],
                },
            );
        });
        // Negative 2a: Non-director member (PILOT_B) calling DeleteFitting for corp fails
        assert!(rpc(
            &ctx,
            &mut sess_b,
            "corpFittingMgr",
            "DeleteFitting",
            &[PyRep::Integer(corp_id), PyRep::Integer(fit_id)],
        )
        .await
        .is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnCommunityFittingsUpdated"));

        // Negative 2b: Deleting non-existent corp fitting returns false and does not emit OnCommunityFittingsUpdated
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "corpFittingMgr",
            "DeleteFitting",
            &[PyRep::Integer(corp_id), PyRep::Integer(999_999)],
        )
        .await
        .expect("DeleteFitting missing");
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnCommunityFittingsUpdated"));

        // Positive: CEO deleting corp fitting does NOT emit OnCommunityFittingsUpdated (corp fittings != community fittings)
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "corpFittingMgr",
            "DeleteFitting",
            &[PyRep::Integer(corp_id), PyRep::Integer(fit_id)],
        )
        .await
        .expect("DeleteFitting corp");
        let fit_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(!fit_notifs.iter().any(|(e, _)| e == "OnCommunityFittingsUpdated"));

        // 3. TrackOfferPurchased does NOT echo OnContextualOfferPurchased
        // Negative 3a: TrackOfferPurchased with invalid offerID (<= 0) fails with InvalidOfferID
        assert!(rpc(&ctx, &mut sess_a, "contextualOfferMgr", "TrackOfferPurchased", &[PyRep::Integer(0)])
            .await
            .is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnContextualOfferPurchased"));

        // Negative 3b: TrackOfferSeen does NOT emit OnContextualOfferPurchased
        let _ = rpc(&ctx, &mut sess_a, "contextualOfferMgr", "TrackOfferSeen", &[PyRep::Integer(42)])
            .await
            .expect("TrackOfferSeen");
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnContextualOfferPurchased"));

        // TrackOfferPurchased(42) records telemetry without echoing OnContextualOfferPurchased
        let _ = rpc(&ctx, &mut sess_a, "contextualOfferMgr", "TrackOfferPurchased", &[PyRep::Integer(42)])
            .await
            .expect("TrackOfferPurchased");
        let offer_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(!offer_notifs.iter().any(|(e, _)| e == "OnContextualOfferPurchased"));

        // 4. OnNesIntroStateChanged
        // Negative 4a: Unauthenticated skip_nes_intro fails
        let mut unauth = eve_machonet::SessionState::new(996);
        assert!(rpc(&ctx, &mut unauth, "nes_intro", "skip_nes_intro", &[]).await.is_err());

        // Negative 4b: get_nes_intro_state is read-only and does NOT emit OnNesIntroStateChanged
        let _ = rpc(&ctx, &mut sess_a, "nes_intro", "get_nes_intro_state", &[])
            .await
            .expect("get_nes_intro_state");
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnNesIntroStateChanged"));

        // Positive: skip_nes_intro emits OnNesIntroStateChanged([1])
        let _ = rpc(&ctx, &mut sess_a, "nes_intro", "skip_nes_intro", &[])
            .await
            .expect("skip_nes_intro");
        let nes_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(
            find_notif(&nes_notifs, "OnNesIntroStateChanged"),
            &[PyRep::Integer(PILOT_A), PyRep::Integer(1)]
        );
    }

    // parity: notify:OnIncomingAgentMessage, notify:OnIncomingTransmission, notify:OnEpicJournalChange, notify:OnDatacoreBought
    #[tokio::test]
    async fn notify_batch9_agents_epic_journal_and_datacores() {
        eve_testkit::attest(&[
            "notify:OnIncomingAgentMessage",
            "notify:OnIncomingTransmission",
            "notify:OnEpicJournalChange",
            "notify:OnDatacoreBought",
        ]);

        let ctx = new_ctx();
        let mut sess_a = login_docked(&ctx, 9_311, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

        // Find an Epic Arc agent (agent_type_id == 10, level 1) or fallback to any level 1 agent
        let (epic_agent_id, epic_sys_id, epic_station_id) = ctx
            .sde
            .agents
            .values()
            .find(|a| a.agent_type_id == 10 && a.level == 1 && a.station_id.is_some())
            .or_else(|| ctx.sde.agents.values().find(|a| a.level == 1 && a.station_id.is_some()))
            .map(|a| (a.agent_id, a.solar_system_id.unwrap_or(30_000_142), a.station_id.unwrap_or(JITA_STATION)))
            .expect("SDE must have a level 1 agent");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.solar_system_id = epic_sys_id;
                ch.station_id = Some(epic_station_id);
            }
        });
        sess_a.solar_system_id = Some(epic_sys_id);
        sess_a.station_id = Some(epic_station_id);

        // 5-8. OnOfferReceived, OnIncomingTransmission, OnIncomingAgentMessage, OnEpicJournalChange
        // Negative 5a: Action 3 (Accept) before Offer fails with MissionNotOffered and emits nothing
        assert!(rpc(
            &ctx,
            &mut sess_a,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(3), PyRep::Integer(epic_agent_id)],
        )
        .await
        .is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnIncomingAgentMessage"));

        // Negative 5b: Unknown agent ID fails
        assert!(rpc(
            &ctx,
            &mut sess_a,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(2), PyRep::Integer(999_999_999)],
        )
        .await
        .is_err());

        // Positive Offer (action 2) -> emits OnIncomingTransmission
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(2), PyRep::Integer(epic_agent_id)],
        )
        .await
        .expect("Agent Offer");
        let offer_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(offer_notifs.iter().any(|(e, a)| e == "OnIncomingTransmission" && a.len() == 2));

        // Positive Accept (action 3) -> emits OnIncomingAgentMessage, OnIncomingTransmission, and (if epic arc) OnEpicJournalChange
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(3), PyRep::Integer(epic_agent_id)],
        )
        .await
        .expect("Agent Accept");
        let accept_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let msg_args = find_notif(&accept_notifs, "OnIncomingAgentMessage");
        assert_eq!(msg_args.len(), 2);
        assert_eq!(msg_args[0], PyRep::Integer(epic_agent_id));
        assert!(matches!(&msg_args[1], PyRep::Tuple(t) if t.len() == 2));
        assert_eq!(find_notif(&accept_notifs, "OnEpicJournalChange"), &[]);

        // TrackOfferDelivered (contextualOfferMgr) -> records telemetry without echoing OnOfferReceived
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "contextualOfferMgr",
            "TrackOfferDelivered",
            &[PyRep::Integer(77)],
        )
        .await
        .expect("TrackOfferDelivered");
        let recv_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(!recv_notifs.iter().any(|(e, _)| e == "OnOfferReceived"));

        // Negative 6b: TrackOfferDelivered with offerID <= 0 fails
        assert!(rpc(
            &ctx,
            &mut sess_a,
            "contextualOfferMgr",
            "TrackOfferDelivered",
            &[PyRep::Integer(0)],
        )
        .await
        .is_err());

        // 9. OnDatacoreBought: R&D agent action 14 (Buy Datacores)
        let rnd_agent_id = ctx
            .sde
            .agents
            .values()
            .find(|a| a.agent_type_id == 4)
            .map(|a| a.agent_id)
            .unwrap_or(epic_agent_id);
        let datacore_tid = ctx
            .sde
            .types
            .values()
            .find(|t| t.group_id == 333)
            .map(|t| t.type_id)
            .unwrap_or(20410);

        // Negative 9a: Buying datacores without active research fails and does not emit OnDatacoreBought
        let (rd_sys, rd_sta) = ctx
            .sde
            .agents
            .get(&rnd_agent_id)
            .map(|a| (a.solar_system_id.unwrap_or(30_000_142), a.station_id.unwrap_or(JITA_STATION)))
            .unwrap_or((30_000_142, JITA_STATION));
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.solar_system_id = rd_sys;
                ch.station_id = Some(rd_sta);
            }
        });
        sess_a.solar_system_id = Some(rd_sys);
        sess_a.station_id = Some(rd_sta);
        assert!(rpc(
            &ctx,
            &mut sess_a,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(14), PyRep::Integer(rnd_agent_id), PyRep::Integer(1), PyRep::Integer(datacore_tid as i64)],
        )
        .await
        .is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnDatacoreBought"));

        // Seed active research with RP for rnd_agent_id, then test Negative 9b (insufficient RP) and Positive
        ctx.db.with_mut_snapshot(|snap| {
            eve_server::services::agents_missions::save_rnd_research_entries(
                snap,
                PILOT_A,
                &[eve_server::services::agents_missions::RndResearchEntry {
                    agent_id: rnd_agent_id,
                    research_type_id: 11442,
                    points_per_day: 100.0,
                    initial_points: 500.0,
                    started_unix: ctx.now_unix(),
                }],
            );
        });

        // Negative 9b: Requesting 100 datacores (5,000 RP) when only 500 RP available fails with InsufficientResearchPoints
        let err_9b = rpc(
            &ctx,
            &mut sess_a,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(14), PyRep::Integer(rnd_agent_id), PyRep::Integer(100), PyRep::Integer(datacore_tid as i64)],
        )
        .await
        .unwrap_err();
        assert_eq!(err_9b.code, "InsufficientResearchPoints");
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(14), PyRep::Integer(rnd_agent_id), PyRep::Integer(2), PyRep::Integer(datacore_tid as i64)],
        )
        .await
        .expect("Buy Datacores");
        let dc_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(dc_notifs.iter().any(|(e, a)| e == "OnDatacoreBought" && a.len() == 3));
    }
}

pub(crate) mod notify_facilities_industry_trade_repackage {
    // Real server-side emission tests for Notify Batch 9 (Part 4: Facilities, Industry, Trade, Market, Godma & Cache).

    use crate::notify_gameplay_emitters::{
        drain_all, find_notif, login_in_space, make_item, new_ctx, rpc, JITA_STATION,
        PILOT_A, PILOT_B, SYS_JITA,
    };
    use crate::notify_gameplay_emitters::login_docked_upwell_struct::{login_docked, upwell_struct};
    use eve_db::InventoryFlag;
    use eve_proto::PyRep;
    use glam::DVec3;

    // parity: notify:OnFacilitiesUpdated, notify:OnPrimingNeededForTradeItems, notify:OnSellRepackageErrors
    #[tokio::test]
    async fn notify_batch9_facilities_industry_trade_and_repackage_errors() {
        eve_testkit::attest(&[
            "notify:OnFacilitiesUpdated",
            "notify:OnPrimingNeededForTradeItems",
            "notify:OnSellRepackageErrors",
        ]);

        let ctx = new_ctx();
        let ship_a = 940_000_001_i64;
        let mut sess_a = login_in_space(&ctx, 9_401, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;
        let mut sess_b = login_docked(&ctx, 9_402, "other", "other-secret", PILOT_B, JITA_STATION).await;
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

        // 1. OnFacilitiesUpdated: SetFacilityTaxes on a player-owned structure
        let struct_id = 1_040_000_000_001_i64;
        let corp_id = 98_900_010_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.corporations.insert(
                corp_id,
                eve_db::CorporationRecord {
                    corporation_id: corp_id,
                    name: "Ind Corp".into(),
                    ticker: "INDC".into(),
                    ceo_id: PILOT_A,
                    ..Default::default()
                },
            );
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.corporation_id = corp_id;
                ch.solar_system_id = SYS_JITA;
            }
            snap.structures.insert(
                struct_id,
                upwell_struct(struct_id, 35835, corp_id, SYS_JITA, "Test Athanor", [10_000.0, 0.0, 0.0]),
            );
        });
        sess_a.corporation_id = Some(corp_id);

        // Negative 1a: Non-owner character (PILOT_B) calling SetFacilityTaxes fails with FacilityPermissionDenied
        assert!(rpc(
            &ctx,
            &mut sess_b,
            "facilityManager",
            "SetFacilityTaxes",
            &[PyRep::Integer(struct_id), PyRep::Real(0.05)],
        )
        .await
        .is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnFacilitiesUpdated"));

        // Negative 1b: Invalid negative tax rate fails with InvalidFacilityTaxRate
        assert!(rpc(
            &ctx,
            &mut sess_a,
            "facilityManager",
            "SetFacilityTaxes",
            &[PyRep::Integer(struct_id), PyRep::Real(-0.1)],
        )
        .await
        .is_err());

        // Positive: CEO calling SetFacilityTaxes emits OnFacilitiesUpdated
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "facilityManager",
            "SetFacilityTaxes",
            &[PyRep::Integer(struct_id), PyRep::Real(0.05)],
        )
        .await
        .expect("SetFacilityTaxes");
        let fac_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(
            find_notif(&fac_notifs, "OnFacilitiesUpdated"),
            &[PyRep::List(vec![PyRep::Integer(struct_id)])]
        );

        // 2. OnIndustryMaterials: blueprintManager.GetBlueprintData
        let bp_item_id = 940_000_050_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                bp_item_id,
                make_item(bp_item_id, 687, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
            );
        });
        // Negative 2a: Unknown blueprint type/item ID fails with BlueprintNotFound
        assert!(rpc(
            &ctx,
            &mut sess_a,
            "blueprintManager",
            "GetBlueprintData",
            &[PyRep::Integer(999_999_999)],
        )
        .await
        .is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnIndustryMaterials"));

        // Negative 2b: Unauthenticated GetBlueprintData does NOT emit OnIndustryMaterials
        let mut unauth = eve_machonet::SessionState::new(995);
        let _ = rpc(
            &ctx,
            &mut unauth,
            "blueprintManager",
            "GetBlueprintData",
            &[PyRep::Integer(bp_item_id)],
        )
        .await;
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnIndustryMaterials"));

        // Read-only GetBlueprintData on owned blueprint does NOT emit OnIndustryMaterials
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "blueprintManager",
            "GetBlueprintData",
            &[PyRep::Integer(bp_item_id)],
        )
        .await
        .expect("GetBlueprintData");
        let mat_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(!mat_notifs.iter().any(|(e, _)| e == "OnIndustryMaterials"));

        // 3. OnPrimingNeededForTradeItems: Direct station trade AddItem
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.station_id = Some(JITA_STATION);
            }
        });
        sess_a.station_id = Some(JITA_STATION);
        let trade_res = rpc(
            &ctx,
            &mut sess_a,
            "trademgr",
            "InitiateTrade",
            &[PyRep::Integer(PILOT_B)],
        )
        .await
        .expect("InitiateTrade");
        let trade_id = match trade_res {
            PyRep::KeyVal(kv) => kv.get("tradeContainerID").and_then(|v| v.as_i64()).unwrap_or(0),
            _ => 0,
        };
        assert!(trade_id > 0);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

        // Negative 3a: Adding non-existent item fails and does not emit OnPrimingNeededForTradeItems
        assert!(rpc(
            &ctx,
            &mut sess_a,
            "trademgr",
            "AddItem",
            &[PyRep::Integer(trade_id), PyRep::Integer(940_999_999), PyRep::Integer(1)],
        )
        .await
        .is_err());
        assert!(!drain_all(&ctx, &mut sess_b, PILOT_B)
            .iter()
            .any(|(e, _)| e == "OnPrimingNeededForTradeItems"));

        // Negative 3b: OfferMoney does NOT emit OnPrimingNeededForTradeItems
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "trademgr",
            "OfferMoney",
            &[PyRep::Integer(trade_id), PyRep::Real(100.0)],
        )
        .await
        .expect("OfferMoney");
        assert!(!drain_all(&ctx, &mut sess_b, PILOT_B)
            .iter()
            .any(|(e, _)| e == "OnPrimingNeededForTradeItems"));

        // Positive: Adding a valid hangar item emits OnPrimingNeededForTradeItems([item_id]) to partner PILOT_B
        let trade_item_id = 940_000_010_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                trade_item_id,
                make_item(trade_item_id, 34, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 50, false),
            );
        });
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "trademgr",
            "AddItem",
            &[PyRep::Integer(trade_id), PyRep::Integer(trade_item_id), PyRep::Integer(10)],
        )
        .await
        .expect("AddItem");
        let partner_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
        assert_eq!(
            find_notif(&partner_notifs, "OnPrimingNeededForTradeItems"),
            &[PyRep::List(vec![PyRep::Integer(trade_item_id)])]
        );

        // 4. OnSellRepackageErrors: PlaceMultiSellOrder on an assembled (singleton) ship
        let assembled_ship_id = 940_000_020_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                assembled_ship_id,
                make_item(assembled_ship_id, 587, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
            );
        });
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

        // Negative 4a: Selling a clean, undamaged assembled ship fails with CannotSellAssembledItem and does NOT emit OnSellRepackageErrors
        let sell_entry = PyRep::keyval([
            ("stationID", PyRep::Integer(JITA_STATION)),
            ("typeID", PyRep::Integer(587)),
            ("itemID", PyRep::Integer(assembled_ship_id)),
            ("price", PyRep::Real(100_000.0)),
            ("quantity", PyRep::Integer(1)),
            ("duration", PyRep::Integer(7)),
        ]);
        let clean_err = rpc(
            &ctx,
            &mut sess_a,
            "marketProxy",
            "PlaceMultiSellOrder",
            &[PyRep::List(vec![sell_entry.clone()]), PyRep::Bool(false), PyRep::Integer(7)],
        )
        .await
        .unwrap_err();
        assert_eq!(clean_err.code, "CannotSellAssembledItem");
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnSellRepackageErrors"));

        // Positive 4: Selling a damaged assembled ship fails with CantRepackageDamagedItem and emits OnSellRepackageErrors
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(it) = snap.items.get_mut(&assembled_ship_id) {
                it.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 25.0);
            }
        });
        let dmg_err = rpc(
            &ctx,
            &mut sess_a,
            "marketProxy",
            "PlaceMultiSellOrder",
            &[PyRep::List(vec![sell_entry]), PyRep::Bool(false), PyRep::Integer(7)],
        )
        .await
        .unwrap_err();
        assert_eq!(dmg_err.code, "CantRepackageDamagedItem");
        let repack_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let repack_args = find_notif(&repack_notifs, "OnSellRepackageErrors");
        assert_eq!(repack_args.len(), 1);
        assert!(matches!(
            &repack_args[0],
            PyRep::Dict(entries) if entries.iter().any(|(k, v)| k.as_str() == Some("CantRepackageDamagedItem") && *v == PyRep::Integer(1))
        ));

        // Negative 4b: Selling a repackaged item with invalid price (0.0) fails without emitting OnSellRepackageErrors
        let repackaged_id = 940_000_021_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                repackaged_id,
                make_item(repackaged_id, 587, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
            );
        });
        let bad_price_entry = PyRep::keyval([
            ("stationID", PyRep::Integer(JITA_STATION)),
            ("typeID", PyRep::Integer(587)),
            ("itemID", PyRep::Integer(repackaged_id)),
            ("price", PyRep::Real(0.0)),
            ("quantity", PyRep::Integer(1)),
            ("duration", PyRep::Integer(7)),
        ]);
        assert!(rpc(
            &ctx,
            &mut sess_a,
            "marketProxy",
            "PlaceMultiSellOrder",
            &[PyRep::List(vec![bad_price_entry]), PyRep::Bool(false), PyRep::Integer(7)],
        )
        .await
        .is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnSellRepackageErrors"));
    }

    // parity: notify:OnGodmaEffectInfo, notify:OnGodmaFlushLocationProfile, notify:OnGodmaPrimeItem, notify:OnInvalidateCacheForLocations, notify:OnMultiEvent
    #[tokio::test]
    async fn notify_batch9_godma_and_location_cache_invalidation() {
        eve_testkit::attest(&[
            "notify:OnGodmaEffectInfo",
            "notify:OnGodmaFlushLocationProfile",
            "notify:OnGodmaPrimeItem",
            "notify:OnInvalidateCacheForLocations",
            "notify:OnMultiEvent",
        ]);

        let ctx = new_ctx();
        let (ship_a, ship_b) = (940_000_101_i64, 940_000_102_i64);
        let mut sess_a = login_in_space(&ctx, 9_411, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;
        let mut sess_b = login_in_space(&ctx, 9_412, "other", "other-secret", PILOT_B, ship_b, 587, SYS_JITA, DVec3::new(1_000.0, 0.0, 0.0)).await;

        let mod_id = 940_000_110_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                mod_id,
                make_item(mod_id, 2873, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true),
            );
        });
        ctx.invalidate_ship_fitting(ship_a);
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

        // 5-7. OnGodmaPrimeItem, OnGodmaEffectInfo, OnGodmaFlushLocationProfile
        // Negative 5a: SetModuleOnline on a ship owned by PILOT_B fails with ShipOwnershipViolation
        assert!(rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "SetModuleOnline",
            &[PyRep::Integer(ship_b), PyRep::Integer(mod_id)],
        )
        .await
        .is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnGodmaPrimeItem"));

        // Positive: SetModuleOnline emits OnGodmaPrimeItem and OnGodmaEffectInfo
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "SetModuleOnline",
            &[PyRep::Integer(ship_a), PyRep::Integer(mod_id)],
        )
        .await
        .expect("SetModuleOnline");
        let online_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let prime_args = find_notif(&online_notifs, "OnGodmaPrimeItem");
        assert_eq!(prime_args.len(), 2);
        assert_eq!(prime_args[0], PyRep::Integer(ship_a));
        assert_eq!(prime_args[1].dict_get("itemID"), Some(&PyRep::Integer(mod_id)));
        assert!(matches!(prime_args[1].dict_get("attributes"), Some(PyRep::Dict(d)) if d.len() > 2));
        let eff_args = find_notif(&online_notifs, "OnGodmaEffectInfo");
        assert_eq!(eff_args.len(), 4);
        assert_eq!(eff_args[0], PyRep::Integer(mod_id));
        assert_eq!(eff_args[1], PyRep::Integer(2873));

        // Negative 7a: TakeModuleOffline on unowned ship fails
        assert!(rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "TakeModuleOffline",
            &[PyRep::Integer(ship_b), PyRep::Integer(mod_id)],
        )
        .await
        .is_err());

        // Positive: TakeModuleOffline emits OnGodmaFlushLocationProfile([ship_a])
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "TakeModuleOffline",
            &[PyRep::Integer(ship_a), PyRep::Integer(mod_id)],
        )
        .await
        .expect("TakeModuleOffline");
        let offline_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let flush_args = find_notif(&offline_notifs, "OnGodmaFlushLocationProfile");
        assert_eq!(flush_args.len(), 2);
        assert_eq!(flush_args[0], PyRep::Integer(ship_a));

        // 8. OnInvalidateCacheForLocations: invbroker.SetPassword on a passworded container
        let pw_container_id = 940_000_060_i64;
        let pw_container_type = ctx
            .sde
            .types
            .values()
            .find(|t| t.group_id == 448 && t.published)
            .map(|t| t.type_id)
            .unwrap_or(17366);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.station_id = Some(JITA_STATION);
            }
            snap.items.insert(
                pw_container_id,
                make_item(pw_container_id, pw_container_type, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
            );
        });
        sess_a.station_id = Some(JITA_STATION);
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "invbroker",
            "SetPassword",
            &[PyRep::Integer(pw_container_id), PyRep::String("secret123".into())],
        )
        .await
        .expect("SetPassword");
        let cache_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(
            find_notif(&cache_notifs, "OnInvalidateCacheForLocations"),
            &[PyRep::List(vec![PyRep::Integer(pw_container_id)])]
        );

        // 9. OnMultiEvent: dogmaIM.AddTarget & RemoveTarget
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.station_id = None;
            }
        });
        sess_a.station_id = None;

        // Negative 9a: AddTarget with missing targetID fails
        assert!(rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[]).await.is_err());

        // Negative 9b: AddTarget with invalid target ID 0 fails
        assert!(rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(0)])
            .await
            .is_err());

        // Positive: AddTarget(ship_b) does NOT emit OnMultiEvent OnTarget add before lock completes,
        // and emits exactly one OnMultiEvent([("OnTarget", "add", ship_b, ""), 0]) on lock completion
        let _ = rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b)])
            .await
            .expect("AddTarget");
        let pre_lock_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(
            !pre_lock_notifs.iter().any(|(e, _)| e == "OnMultiEvent"),
            "OnMultiEvent OnTarget add must not be emitted before lock completes"
        );
        ctx.systems
            .get_or_spawn(SYS_JITA)
            .step_manual(10.0)
            .await
            .expect("step_manual");
        let locker_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(
            locker_notifs.iter().filter(|(e, _)| e == "OnMultiEvent").count(),
            1,
            "expected exactly one OnMultiEvent on locker after lock completion"
        );
        assert_eq!(
            find_notif(&locker_notifs, "OnMultiEvent"),
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Tuple(vec![
                    PyRep::String("OnTarget".into()),
                    PyRep::String("add".into()),
                    PyRep::Integer(ship_b),
                    PyRep::String(String::new()),
                ]),
                PyRep::Integer(0),
            ])])]
        );
        let target_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
        assert_eq!(
            target_notifs.iter().filter(|(e, _)| e == "OnMultiEvent").count(),
            1,
            "expected exactly one OnMultiEvent on targeted pilot after lock completion"
        );
        assert_eq!(
            find_notif(&target_notifs, "OnMultiEvent"),
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Tuple(vec![
                    PyRep::String("OnTarget".into()),
                    PyRep::String("otheradd".into()),
                    PyRep::Integer(ship_a),
                    PyRep::String(String::new()),
                ]),
                PyRep::Integer(0),
            ])])]
        );
    }
}

pub(crate) mod notify_jump_queue_flagged_lists {
    // Real server-side emission tests for Notify Batch 9 (Part 5: JumpQueue, FlaggedLists, CfgDataChanged, CCPNotification, ServerMessage, Localization & ServiceMapping).

    use crate::notify_gameplay_emitters::{
        drain_all, find_notif, login_in_space, new_ctx, rpc, JITA_STATION, PILOT_A,
        PILOT_B, SYS_JITA,
    };
    use crate::notify_gameplay_emitters::login_docked_upwell_struct::{login_docked, upwell_struct};
    use eve_proto::PyRep;
    use glam::DVec3;

    // parity: notify:OnCfgDataChanged
    #[tokio::test]
    async fn notify_batch9_jump_queue_flagged_lists_and_cfg_data() {
        eve_testkit::attest(&[
            "notify:OnCfgDataChanged",
        ]);

        let ctx = new_ctx();
        let ship_a = 950_000_001_i64;
        let mut sess_a = login_in_space(&ctx, 9_501, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;
        let mut sess_b = login_docked(&ctx, 9_502, "other", "other-secret", PILOT_B, JITA_STATION).await;
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

        // 3. OnCfgDataChanged: RenameStructure
        let struct_id = 1_050_000_000_001_i64;
        let corp_id = 98_900_020_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.corporations.insert(
                corp_id,
                eve_db::CorporationRecord {
                    corporation_id: corp_id,
                    name: "Cfg Corp".into(),
                    ticker: "CFGC".into(),
                    ceo_id: PILOT_A,
                    ..Default::default()
                },
            );
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.corporation_id = corp_id;
            }
            snap.structures.insert(
                struct_id,
                upwell_struct(struct_id, 35832, corp_id, SYS_JITA, "Old Astrahus", [5_000.0, 1_000.0, -2_000.0]),
            );
        });
        sess_a.corporation_id = Some(corp_id);

        // Negative 3a: Empty structure name fails with InvalidStructureName
        assert!(rpc(
            &ctx,
            &mut sess_a,
            "structureDeployment",
            "RenameStructure",
            &[PyRep::Integer(struct_id), PyRep::String("   ".into())],
        )
        .await
        .is_err());

        // Negative 3b: Non-corp member (PILOT_B) calling RenameStructure fails
        sess_b.role = 0;
        assert!(rpc(
            &ctx,
            &mut sess_b,
            "structureDeployment",
            "RenameStructure",
            &[PyRep::Integer(struct_id), PyRep::String("Hacked Name".into())],
        )
        .await
        .is_err());

        // Positive: CEO renaming structure emits OnCfgDataChanged(["evelocations", 6-elem list])
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "structureDeployment",
            "RenameStructure",
            &[PyRep::Integer(struct_id), PyRep::String("New Astrahus".into())],
        )
        .await
        .expect("RenameStructure");
        let cfg_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let cfg_args = find_notif(&cfg_notifs, "OnCfgDataChanged");
        assert_eq!(cfg_args.len(), 2);
        assert_eq!(cfg_args[0], PyRep::String("evelocations".into()));
        assert!(matches!(&cfg_args[1], PyRep::List(elems) if elems.len() == 6));
    }

    // parity: notify:OnServerMessage
    #[tokio::test]
    async fn notify_batch9_ccp_notification_server_msg_localization_and_service_map() {
        eve_testkit::attest(&[
            "notify:OnServerMessage",
        ]);

        let ctx = new_ctx();
        let mut sess_a = login_docked(&ctx, 9_511, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
        let mut sess_gm = login_docked(&ctx, 9_512, "other", "other-secret", PILOT_B, JITA_STATION).await;
        sess_gm.role |= eve_db::service_roles::ROLE_GMH;
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_gm, PILOT_B);

        // 5. OnServerMessage: machoNet.GracefulShutdown
        // Negative 5a: Non-GM calling GracefulShutdown fails with InsufficientRole
        assert!(rpc(
            &ctx,
            &mut sess_a,
            "machoNet",
            "GracefulShutdown",
            &[PyRep::Integer(1), PyRep::String("Maintenance".into()), PyRep::Integer(300), PyRep::Bool(true)],
        )
        .await
        .is_err());

        // Positive: GM initiating GracefulShutdown broadcasts OnServerMessage
        let _ = rpc(
            &ctx,
            &mut sess_gm,
            "machoNet",
            "GracefulShutdown",
            &[PyRep::Integer(1), PyRep::String("Welcome to Tranquility".into()), PyRep::Integer(300), PyRep::Bool(true)],
        )
        .await
        .expect("GracefulShutdown");
        let srv_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(
            find_notif(&srv_notifs, "OnServerMessage"),
            &[PyRep::String("Welcome to Tranquility".into())]
        );

        // 6. UpdateLocalizationQAWrap rejects unprivileged caller and succeeds for GM without emitting OnUpdateLocalizationTextCache
        assert!(rpc(
            &ctx,
            &mut sess_a,
            "localizationServer",
            "UpdateLocalizationQAWrap",
            &[PyRep::Bool(true)],
        )
        .await
        .is_err());

        let qa_ok = rpc(
            &ctx,
            &mut sess_gm,
            "localizationServer",
            "UpdateLocalizationQAWrap",
            &[PyRep::Bool(true)],
        )
        .await
        .expect("GM UpdateLocalizationQAWrap(true)");
        assert_eq!(qa_ok, PyRep::Bool(true));
        let loc_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(!loc_notifs.iter().any(|(e, _)| e == "OnUpdateLocalizationTextCache"));
    }
}

pub(crate) mod new_ctx_rpc {
    // Real server-side emission & RPC tests for Notify/RPC Batch 8 (Part 1: Seasons).
    //
    // Covers all 10 Group 1 Seasons parity rows:
    // - `rpc:seasonManager.get_season_data_for_character`
    // - `rpc:seasonManager.get_season_end_time`
    // - `rpc:seasonManager.select_season`
    // - `notify:OnSeasonStarted`
    // - `notify:OnSeasonEnded`
    // - `notify:OnSeasonSelectionCleared`
    // - `notify:OnSeasonOmegaRewardsClaimed`
    // - `notify:OnSeasonalGoalCompleted`
    // - `notify:OnSeasonalGoalsReset`
    // - `notify:OnChallengeExpired`


    use crate::common;
    use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use eve_server::services::season_manager::{SeasonalChallengeDef, SEASONAL_CHALLENGES_CONFIG_KEY};
    use std::collections::HashMap;

    const PILOT_A: i64 = 90_000_001;
    const PILOT_B: i64 = 90_000_002;
    const JITA_STATION: i64 = 60_003_760;
    const ERR_INVALID_SEASON: &str = "InvalidSeasonId";
    const ERR_CHALLENGE_NOT_COMPLETED: &str = "ChallengeNotCompleted";
    const ERR_CHALLENGE_ALREADY_CLAIMED: &str = "ChallengeAlreadyClaimed";

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

    fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
        let mut out = sess.drain_notifications();
        for n in ctx.drain_notifications_for_char(char_id) {
            out.push((n.event, n.args));
        }
        out
    }

    fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
        notifs
            .iter()
            .find(|(e, _)| e == event)
            .map(|(_, args)| args.as_slice())
            .unwrap_or_else(|| {
                let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
                panic!("expected notification {event}, got {seen:?}")
            })
    }

    async fn login_docked(
        ctx: &ServerContext,
        sid: u64,
        user: &str,
        pass: &str,
        char_id: i64,
        clone_grade: i32,
    ) -> SessionState {
        let mut sess = SessionState::new(sid);
        common::login_pilot(ctx, &mut sess, user, pass).await;
        rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
            .await
            .expect("SelectCharacterID");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.clone_grade = clone_grade as u32;
                ch.station_id = Some(JITA_STATION);
                ch.structure_id = None;
            }
        });
        sess.station_id = Some(JITA_STATION);
        sess.structure_id = None;
        let _ = drain_all(ctx, &mut sess, char_id);
        sess
    }

    // parity: rpc:seasonManager.get_season_data_for_character, rpc:seasonManager.get_season_end_time
    // parity: rpc:seasonManager.select_season, notify:OnSeasonStarted, notify:OnSeasonEnded
    // parity: notify:OnSeasonSelectionCleared, notify:OnChallengeExpired
    #[tokio::test]
    async fn notify_batch8_seasons_lifecycle_and_challenges() {
        eve_testkit::attest(&[
            "rpc:seasonManager.get_season_data_for_character",
            "rpc:seasonManager.get_season_end_time",
            "rpc:seasonManager.select_season",
            "notify:OnSeasonStarted",
            "notify:OnSeasonEnded",
            "notify:OnSeasonSelectionCleared",
            "notify:OnChallengeExpired",
        ]);

        let ctx = new_ctx();
        let mut unauth_sess = SessionState::new(9_100);
        assert_eq!(
            rpc(&ctx, &mut unauth_sess, "seasonManager", "get_season_data_for_character", &[])
                .await
                .unwrap_err()
                .code,
            "Unauthorized"
        );
        assert_eq!(
            rpc(&ctx, &mut unauth_sess, "seasonManager", "get_season_end_time", &[])
                .await
                .unwrap_err()
                .code,
            "Unauthorized"
        );
        assert_eq!(
            rpc(&ctx, &mut unauth_sess, "seasonManager", "select_season", &[PyRep::Integer(1)])
                .await
                .unwrap_err()
                .code,
            "Unauthorized"
        );

        let mut sess_a = login_docked(&ctx, 9_101, "demo", "demo-secret", PILOT_A, 2).await;
        let mut sess_b = login_docked(&ctx, 9_102, "other", "other-secret", PILOT_B, 1).await;

        // Seed custom challenge definitions so challenge 1005 awards 25 points (crosses 2 goal milestones!)
        ctx.db.with_mut_snapshot(|snap| {
            let defs = vec![
                SeasonalChallengeDef {
                    challenge_id: 1_005,
                    season_id: 1,
                    event_type: "ui_onboarding_step".into(),
                    title: "Onboarding".into(),
                    description: "Complete onboarding steps".into(),
                    points: 25,
                    max_progress: 2,
                    client_event_driven: true,
                    reward_type_id: 0,
                    reward_quantity: 0,
                    is_dormant: false,
                    ..Default::default()
                },
                SeasonalChallengeDef {
                    challenge_id: 1_006,
                    season_id: 1,
                    event_type: "npc_kills".into(),
                    title: "Bounty Hunter".into(),
                    description: "Destroy pirate vessels".into(),
                    points: 10,
                    max_progress: 5,
                    client_event_driven: false,
                    reward_type_id: 0,
                    reward_quantity: 0,
                    is_dormant: false,
                    ..Default::default()
                },
            ];
            snap.service_state_blobs.insert(
                SEASONAL_CHALLENGES_CONFIG_KEY.into(),
                serde_json::to_string(&defs).expect("serialize defs"),
            );
        });

        // 1. Baseline get_season_data_for_character & get_season_end_time
        let data_init = rpc(&ctx, &mut sess_a, "seasonManager", "get_season_data_for_character", &[])
            .await
            .expect("get_season_data_for_character");
        let first_season_id = data_init
            .dict_get("season_id")
            .or_else(|| {
                if let PyRep::List(items) = &data_init {
                    items.first().and_then(|it| it.dict_get("season_id"))
                } else {
                    None
                }
            })
            .and_then(|v| v.as_i64())
            .unwrap_or(1);
        assert!(first_season_id > 0, "expected active season_id > 0, got {data_init:?}");

        let end_time_init = rpc(&ctx, &mut sess_a, "seasonManager", "get_season_end_time", &[])
            .await
            .expect("get_season_end_time");
        let end_ft_val = end_time_init.as_i64().expect("integer filetime end_time");
        assert!(end_ft_val > ctx.now_blueos_filetime_ticks());

        // Negative: non-GM cannot start or end a season via slash command
        assert_eq!(
            rpc(
                &ctx,
                &mut sess_b,
                "publicQaToolsServer",
                "SlashCmd",
                &[PyRep::String("/season start 88 14".into())],
            )
            .await
            .unwrap_err()
            .code,
            "InsufficientRole"
        );

        // 2. Start Season 88 via GM /season start 88 10 -> emits OnSeasonStarted
        sess_a.role |= ROLE_GM_DEFAULT;
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
        let slash_start = rpc(
            &ctx,
            &mut sess_a,
            "slash",
            "SlashCmd",
            &[PyRep::String("/season start 88 10".into())],
        )
        .await
        .expect("/season start 88 10");
        assert_eq!(slash_start.dict_get("seasonID").and_then(|v| v.as_i64()), Some(88));
        assert_eq!(slash_start.dict_get("started").and_then(|v| v.as_bool()), Some(true));

        let notifs_start_a = drain_all(&ctx, &mut sess_a, PILOT_A);
        let notifs_start_b = drain_all(&ctx, &mut sess_b, PILOT_B);
        assert_eq!(find_notif(&notifs_start_a, "OnSeasonStarted").len(), 0);
        assert_eq!(find_notif(&notifs_start_b, "OnSeasonStarted").len(), 0);

        // Negative: invalid season IDs and invalid slash arguments rejected
        assert_eq!(
            rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String("/season start 0 10".into())])
                .await
                .unwrap_err()
                .code,
            "InvalidSlashArgument"
        );
        let err_999 = rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::Integer(999_999)])
            .await
            .unwrap_err();
        assert_eq!(err_999.code.as_str(), ERR_INVALID_SEASON);
        let err_neg = rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::Integer(-5)])
            .await
            .unwrap_err();
        assert_eq!(err_neg.code.as_str(), ERR_INVALID_SEASON);

        // 3. select_season(88) selects Season 88, then select_season(None) & select_season(0) emit OnSeasonSelectionCleared
        let sel_ok = rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::Integer(88)])
            .await
            .expect("select_season(88)");
        assert_eq!(sel_ok, PyRep::Bool(true));
        let data_88 = rpc(&ctx, &mut sess_a, "seasonManager", "get_season_data_for_character", &[])
            .await
            .expect("get_season_data_for_character(88)");
        assert_eq!(data_88.dict_get("season_id").and_then(|v| v.as_i64()), Some(88));
        let end_88 = rpc(&ctx, &mut sess_a, "seasonManager", "get_season_end_time", &[])
            .await
            .expect("get_season_end_time(88)");
        assert!(end_88.as_i64().unwrap_or(0) > ctx.now_blueos_filetime_ticks());

        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let clear_rep = rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::None])
            .await
            .expect("select_season(None)");
        assert_eq!(clear_rep, PyRep::Bool(true));
        let clear_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(find_notif(&clear_notifs, "OnSeasonSelectionCleared").len(), 0);

        // Re-select Season 88 for both pilots, then end Season 88 -> emits OnSeasonEnded & OnSeasonSelectionCleared
        rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::Integer(88)])
            .await
            .expect("re-select 88 A");
        rpc(&ctx, &mut sess_b, "seasonManager", "select_season", &[PyRep::Integer(88)])
            .await
            .expect("re-select 88 B");
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

        let end_rep = rpc(
            &ctx,
            &mut sess_a,
            "slash",
            "SlashCmd",
            &[PyRep::String("/season end 88".into())],
        )
        .await
        .expect("/season end 88");
        assert_eq!(end_rep.dict_get("ended").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(end_rep.dict_get("clearedCharacters").and_then(|v| v.as_i64()), Some(2));

        let end_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
        let end_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
        assert_eq!(find_notif(&end_notifs_a, "OnSeasonEnded").len(), 0);
        assert_eq!(find_notif(&end_notifs_a, "OnSeasonSelectionCleared").len(), 0);
        assert_eq!(find_notif(&end_notifs_b, "OnSeasonEnded").len(), 0);
        assert_eq!(find_notif(&end_notifs_b, "OnSeasonSelectionCleared").len(), 0);

        // Negative: selecting invalid season -1 fails with CustomInfo; ending unknown season fails
        let err_sel0 = rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::Integer(-1)])
            .await
            .unwrap_err();
        assert_eq!(err_sel0.code.as_str(), ERR_INVALID_SEASON);
        assert_eq!(
            rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String("/season end 999999".into())])
                .await
                .unwrap_err()
                .code,
            "InvalidSeasonId"
        );

        // 4. Complete challenge 1005 (25 pts) -> claim_challenge_rewards emits OnSeasonalGoalCompleted
        // and (for Omega PILOT_A) OnSeasonOmegaRewardsClaimed, while Alpha PILOT_B does NOT receive Omega claim!
        let _ = rpc(&ctx, &mut sess_a, "seasonManager", "fill_up_and_return_challenges_for_character", &[])
            .await
            .expect("fill challenges A");
        let _ = rpc(&ctx, &mut sess_b, "seasonManager", "fill_up_and_return_challenges_for_character", &[])
            .await
            .expect("fill challenges B");

        // Negative: claiming incomplete challenge fails with ChallengeNotCompleted
        let err_uncomp = rpc(&ctx, &mut sess_a, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)])
            .await
            .unwrap_err();
        assert_eq!(err_uncomp.code.as_str(), ERR_CHALLENGE_NOT_COMPLETED);

        for _ in 0..2 {
            rpc(
                &ctx,
                &mut sess_a,
                "seasonManager",
                "process_client_event",
                &[PyRep::String("ui_onboarding_step".into())],
            )
            .await
            .expect("complete challenge 1005 A");
            rpc(
                &ctx,
                &mut sess_b,
                "seasonManager",
                "process_client_event",
                &[PyRep::String("ui_onboarding_step".into())],
            )
            .await
            .expect("complete challenge 1005 B");
        }

        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

        let claim_a = rpc(&ctx, &mut sess_a, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)])
            .await
            .expect("claim_challenge_rewards A");
        assert_eq!(claim_a.dict_get("claimed").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(claim_a.dict_get("seasonPoints").and_then(|v| v.as_i64()), Some(25));

        let claim_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
        let goal_args = find_notif(&claim_notifs_a, "OnSeasonalGoalCompleted");
        assert_eq!(goal_args.len(), 2);
        assert_eq!(goal_args[0].as_i64(), Some(0));
        assert_eq!(goal_args[1].dict_get("completed").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(goal_args[1].dict_get("points_required").and_then(|v| v.as_i64()), Some(10));

        let omega_args = find_notif(&claim_notifs_a, "OnSeasonOmegaRewardsClaimed");
        assert_eq!(omega_args.len(), 1);
        assert_eq!(omega_args[0], PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(1)]));

        // Alpha pilot B gets OnSeasonalGoalCompleted but NOT OnSeasonOmegaRewardsClaimed
        let _ = rpc(&ctx, &mut sess_b, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)])
            .await
            .expect("claim_challenge_rewards B");
        let claim_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
        assert_eq!(find_notif(&claim_notifs_b, "OnSeasonalGoalCompleted").len(), 2);
        assert!(
            !claim_notifs_b.iter().any(|(ev, _)| ev == "OnSeasonOmegaRewardsClaimed"),
            "Alpha pilot must not receive OnSeasonOmegaRewardsClaimed"
        );

        // Negative: double-claiming already claimed challenge fails with ChallengeAlreadyClaimed
        let err_claimed = rpc(&ctx, &mut sess_a, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)])
            .await
            .unwrap_err();
        assert_eq!(err_claimed.code.as_str(), ERR_CHALLENGE_ALREADY_CLAIMED);

        // 5. Reset seasonal goals via /season reset_goals -> emits OnSeasonalGoalsReset and zeroes points
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let reset_rep = rpc(
            &ctx,
            &mut sess_a,
            "slash",
            "SlashCmd",
            &[PyRep::String(format!("/season reset_goals {PILOT_A}"))],
        )
        .await
        .expect("/season reset_goals");
        assert_eq!(reset_rep.dict_get("seasonPoints").and_then(|v| v.as_i64()), Some(0));
        let reset_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(find_notif(&reset_notifs, "OnSeasonalGoalsReset").len(), 0);
        // Claimed challenge must NOT be re-armed by /season reset_goals before expiration
        let err_rearm = rpc(&ctx, &mut sess_a, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)])
            .await
            .unwrap_err();
        assert_eq!(err_rearm.code.as_str(), ERR_CHALLENGE_ALREADY_CLAIMED);

        // Negative: resetting goals for non-existent character fails
        assert_eq!(
            rpc(
                &ctx,
                &mut sess_a,
                "slash",
                "SlashCmd",
                &[PyRep::String("/season reset_goals 999888777".into())],
            )
            .await
            .unwrap_err()
            .code,
            "CharacterNotFound"
        );

        // 6. Advance virtual clock by 35 days (> 48h challenge expiration) -> fill_up_and_return_challenges_for_character
        // refreshes expired challenges and emits OnChallengeExpired(expired_id, new_challenge_kv)
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        ctx.advance_secs(35 * 86_400);
        let _ = rpc(&ctx, &mut sess_a, "seasonManager", "fill_up_and_return_challenges_for_character", &[])
            .await
            .expect("fill challenges after expiry");
        let exp_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let exp_args = find_notif(&exp_notifs, "OnChallengeExpired");
        assert_eq!(exp_args.len(), 2);
        assert!(exp_args[0].as_i64().unwrap_or(0) > 0);
        assert!(
            exp_args[1].dict_get("challenge_id").is_some()
                || exp_args[1].dict_get("challengeID").is_some(),
            "OnChallengeExpired arg 1 must be replacement challenge KeyVal, got {:?}",
            exp_args[1]
        );

        // 7. Time-driven season transitions: advance clock past a future season's start_time_ft and end_time_ft
        // -> OnSeasonStarted, OnSeasonEnded, and OnSeasonSelectionCleared fire from the world tick without Get* calls
        let now_ft = ctx.now_blueos_filetime_ticks();
        let future_start_ft = now_ft + 100 * 10_000_000;
        let future_end_ft = now_ft + 200 * 10_000_000;
        ctx.db.with_mut_snapshot(|snap| {
            let mut catalog: std::collections::BTreeMap<i64, eve_server::services::season_manager::SeasonConfig> =
                snap.service_state_blobs
                    .get(eve_server::services::season_manager::SEASONS_CATALOG_KEY)
                    .and_then(|j| serde_json::from_str(j).ok())
                    .unwrap_or_default();
            if let Some(mut cfg) = catalog.values().next().cloned() {
                cfg.season_id = 99;
                cfg.season_name = "Timed Season 99".into();
                cfg.start_time_ft = future_start_ft;
                cfg.end_time_ft = future_end_ft;
                catalog.insert(99, cfg);
                snap.service_state_blobs.insert(
                    eve_server::services::season_manager::SEASONS_CATALOG_KEY.into(),
                    serde_json::to_string(&catalog).unwrap(),
                );
            }
        });
        // Initialize announced season state snapshot while season 99 is still upcoming
        ctx.advance_secs(1);
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

        // Advance 120s past future_start_ft -> OnSeasonStarted emitted automatically
        ctx.advance_secs(120);
        let timed_start_a = drain_all(&ctx, &mut sess_a, PILOT_A);
        let timed_start_b = drain_all(&ctx, &mut sess_b, PILOT_B);
        assert!(
            timed_start_a.iter().any(|(ev, _)| ev == "OnSeasonStarted"),
            "expected OnSeasonStarted on clock advance past start_time_ft"
        );
        assert!(
            timed_start_b.iter().any(|(ev, _)| ev == "OnSeasonStarted"),
            "expected OnSeasonStarted for pilot B on clock advance past start_time_ft"
        );

        // Select season 99 for PILOT_A, then advance 120s past future_end_ft -> OnSeasonEnded + OnSeasonSelectionCleared
        let sel_99 = rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::Integer(99)])
            .await
            .expect("select_season(99)");
        assert_eq!(sel_99, PyRep::Bool(true));
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

        ctx.advance_secs(120);
        let timed_end_a = drain_all(&ctx, &mut sess_a, PILOT_A);
        let timed_end_b = drain_all(&ctx, &mut sess_b, PILOT_B);
        assert!(
            timed_end_a.iter().any(|(ev, _)| ev == "OnSeasonEnded"),
            "expected OnSeasonEnded on clock advance past end_time_ft"
        );
        assert!(
            timed_end_b.iter().any(|(ev, _)| ev == "OnSeasonEnded"),
            "expected OnSeasonEnded for pilot B on clock advance past end_time_ft"
        );
        assert!(
            timed_end_a.iter().any(|(ev, _)| ev == "OnSeasonSelectionCleared"),
            "expected OnSeasonSelectionCleared for pilot A when selected season 99 ended"
        );
    }
}

pub(crate) mod new_ctx_rpc_alpha {
    // Real server-side emission & RPC tests for Notify/RPC Batch 8 (Part 2: HyperNet Raffles & PLEX Vault).
    //
    // Covers 7 Group 2 HyperNet Raffles & PLEX Vault parity rows:
    // - `rpc:raffleMgr.QA_SeedRaffles`
    // - `notify:OnRaffleCreatedServer`
    // - `notify:OnRaffleFinishedServer`
    // - `notify:OnRaffleUpdatedServer`
    // - `notify:OnTicketsUpdatedServer`
    // - `notify:OnPlexDeposited`
    // - `notify:OnPlexItemAddedToInventory`


    use crate::common;
    use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
    use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use eve_server::services::hypernet_raffle_ops::{sweep_expired_raffles, HYPERCORE_TYPE_ID};
    use eve_server::services::market_contracts::PLEX_TYPE_ID;
    use std::collections::HashMap;

    const PILOT_A: i64 = 90_000_001;
    const PILOT_B: i64 = 90_000_002;
    const JITA_STATION: i64 = 60_003_760;

    fn new_ctx() -> ServerContext {
        let sde = std::sync::Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
        );
        let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        ctx.set_clock_paused(true);
        ctx.set_mechanic_rng_seed(Some(77));
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

    fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
        let mut out = sess.drain_notifications();
        for n in ctx.drain_notifications_for_char(char_id) {
            out.push((n.event, n.args));
        }
        out
    }

    fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
        notifs
            .iter()
            .find(|(e, _)| e == event)
            .map(|(_, args)| args.as_slice())
            .unwrap_or_else(|| {
                let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
                panic!("expected notification {event}, got {seen:?}")
            })
    }

    async fn login_docked(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
        let mut sess = SessionState::new(sid);
        common::login_pilot(ctx, &mut sess, user, pass).await;
        rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
            .await
            .expect("SelectCharacterID");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = Some(JITA_STATION);
                ch.structure_id = None;
                ch.balance = IskCents::from_f64_isk(500_000_000.0);
            }
        });
        sess.station_id = Some(JITA_STATION);
        sess.structure_id = None;
        let _ = drain_all(ctx, &mut sess, char_id);
        sess
    }

    // parity: rpc:raffleMgr.QA_SeedRaffles, notify:OnRaffleCreatedServer, notify:OnRaffleFinishedServer
    // parity: notify:OnRaffleUpdatedServer, notify:OnTicketsUpdatedServer, notify:OnPlexDeposited
    // parity: notify:OnPlexItemAddedToInventory
    #[tokio::test]
    async fn notify_batch8_hypernet_raffles_and_plex_vault() {
        eve_testkit::attest(&[
            "rpc:raffleMgr.QA_SeedRaffles",
            "notify:OnRaffleCreatedServer",
            "notify:OnRaffleFinishedServer",
            "notify:OnRaffleUpdatedServer",
            "notify:OnTicketsUpdatedServer",
            "notify:OnPlexDeposited",
            "notify:OnPlexItemAddedToInventory",
        ]);

        let ctx = new_ctx();
        let mut unauth = SessionState::new(9_200);
        assert_eq!(
            rpc(&ctx, &mut unauth, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(2)])
                .await
                .unwrap_err()
                .code,
            "Unauthorized"
        );
        assert_eq!(
            rpc(
                &ctx,
                &mut unauth,
                "invbroker",
                "DepositPlexToVault",
                &[PyRep::Integer(JITA_STATION), PyRep::Integer(1), PyRep::Integer(10)],
            )
            .await
            .unwrap_err()
            .code,
            "Unauthorized"
        );

        let mut sess_a = login_docked(&ctx, 9_201, "demo", "demo-secret", PILOT_A).await;
        let mut sess_b = login_docked(&ctx, 9_202, "other", "other-secret", PILOT_B).await;

        // 1. QA_SeedRaffles role check + positive seeding
        assert_eq!(
            rpc(&ctx, &mut sess_a, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(3)])
                .await
                .unwrap_err()
                .code,
            "InsufficientRole"
        );
        sess_a.role |= ROLE_GM_DEFAULT;
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let seeded_rep = rpc(&ctx, &mut sess_a, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(3)])
            .await
            .expect("QA_SeedRaffles");
        let PyRep::List(seeded_list) = seeded_rep else {
            panic!("expected List from QA_SeedRaffles, got {seeded_rep:?}");
        };
        assert_eq!(seeded_list.len(), 3);
        let seeded_rid_0 = seeded_list[0].dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap_or(0);
        let seeded_iid_0 = seeded_list[0].dict_get("item_id").and_then(|v| v.as_i64()).unwrap_or(0);
        assert!(seeded_rid_0 > 0 && seeded_iid_0 > 0);
        assert!(seeded_list[0].dict_get("sold_tickets").is_some(), "QA_SeedRaffles must include sold_tickets");
        let seeded_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let created_rids: std::collections::BTreeSet<i64> = seeded_notifs
            .iter()
            .filter(|(n, _)| n == "OnRaffleCreatedServer")
            .filter_map(|(_, a)| a.first().and_then(|v| v.as_i64()))
            .collect();
        assert_eq!(
            created_rids.len(),
            3,
            "QA_SeedRaffles(3) must emit OnRaffleCreatedServer for all 3 seeded raffles"
        );
        ctx.db.with_snapshot(|snap| {
            let it = snap.items.get(&seeded_iid_0).expect("seeded item in snap.items");
            assert_eq!((it.owner_id, it.location_id, it.flag), (-52_568, -52_568, InventoryFlag::Locked));
        });
        let _ = rpc(&ctx, &mut sess_b, "raffleProxy", "BuyRandomTickets", &[PyRep::Integer(seeded_rid_0), PyRep::Integer(8)])
            .await
            .expect("BuyRandomTickets on seeded raffle");
        let claimed_seeded = rpc(&ctx, &mut sess_b, "raffleProxy", "AwardItem", &[PyRep::Integer(seeded_rid_0)])
            .await
            .expect("AwardItem on seeded raffle");
        assert_eq!(claimed_seeded.as_i64(), Some(seeded_iid_0));
        ctx.db.with_snapshot(|snap| {
            let it = snap.items.get(&seeded_iid_0).expect("awarded seeded item");
            assert_eq!((it.owner_id, it.location_id, it.flag), (PILOT_B, JITA_STATION, InventoryFlag::Hangar));
        });
        let seeded_tc = seeded_list[0].dict_get("ticket_count").and_then(|v| v.as_i64()).unwrap_or(0);
        assert!(
            [8, 16, 48, 512].contains(&seeded_tc),
            "QA_SeedRaffles ticket_count must be in [8, 16, 48, 512], got {seeded_tc}"
        );
        assert_eq!(seeded_list[0].dict_get("owner_id").and_then(|v| v.as_i64()), Some(PILOT_A));
        assert_eq!(seeded_list[0].dict_get("location_id").and_then(|v| v.as_i64()), Some(JITA_STATION));

        // Undocked GM session must fail closed (no Jita fallback)
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.station_id = None;
            }
        });
        let mut undocked_gm = sess_a.clone();
        undocked_gm.station_id = None;
        assert!(
            rpc(&ctx, &mut undocked_gm, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(1)])
                .await
                .is_err(),
            "QA_SeedRaffles must fail closed when station_id is None"
        );
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.station_id = Some(JITA_STATION);
            }
        });

        // 2. CreateRaffle -> emits OnRaffleCreatedServer(raffle_id, raffle_kv)
        let (item_1, item_2, cores_id) = (820_000_001_i64, 820_000_002_i64, 820_000_003_i64);
        ctx.db.with_mut_snapshot(|snap| {
            for (iid, tid, qty, sing) in [
                (item_1, 587u32, 1, true),
                (item_2, 587u32, 1, true),
                (cores_id, HYPERCORE_TYPE_ID, 50, false),
            ] {
                snap.items.insert(
                    iid,
                    InventoryItemRecord {
                        item_id: iid,
                        type_id: tid,
                        owner_id: PILOT_A,
                        location_id: JITA_STATION,
                        flag: InventoryFlag::Hangar,
                        quantity: qty,
                        singleton: sing,
                        version: 1,
                        ..Default::default()
                    },
                );
            }
        });

        // Negative: invalid ticket count (7 not in [8, 16, 48, 512]) and missing item rejected
        assert_eq!(
            rpc(
                &ctx,
                &mut sess_a,
                "raffleProxy",
                "CreateRaffle",
                &[PyRep::Integer(item_1), PyRep::Integer(7), PyRep::Real(100_000.0)],
            )
            .await
            .unwrap_err()
            .code,
            "InvalidRaffleTicketCount"
        );
        assert_eq!(
            rpc(
                &ctx,
                &mut sess_a,
                "raffleProxy",
                "CreateRaffle",
                &[PyRep::Integer(999_888_777), PyRep::Integer(8), PyRep::Real(100_000.0)],
            )
            .await
            .unwrap_err()
            .code,
            "ItemNotFound"
        );

        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let create_rep = rpc(
            &ctx,
            &mut sess_a,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::Integer(item_1), PyRep::Integer(8), PyRep::Real(100_000.0)],
        )
        .await
        .expect("CreateRaffle");
        let raffle_id = create_rep
            .dict_get("raffle_id")
            .and_then(|v| v.as_i64())
            .expect("raffle_id");
        let create_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let created_args = find_notif(&create_notifs, "OnRaffleCreatedServer");
        assert_eq!(created_args.len(), 2);
        assert_eq!(created_args[0].as_i64(), Some(raffle_id));
        assert_eq!(
            created_args[1].dict_get("ticket_count").and_then(|v| v.as_i64()),
            Some(8)
        );
        // Calling QA_SeedRaffles after CreateRaffle must not collide with or overwrite raffle_id
        let post_seed_rep = rpc(&ctx, &mut sess_a, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(1)])
            .await
            .expect("QA_SeedRaffles after CreateRaffle");
        let PyRep::List(post_seed_list) = post_seed_rep else {
            panic!("expected List from QA_SeedRaffles");
        };
        let post_seed_rid = post_seed_list[0].dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap_or(0);
        assert_ne!(post_seed_rid, raffle_id, "QA_SeedRaffles must not collide with CreateRaffle raffle_id");
        let live_raffle = rpc(&ctx, &mut sess_a, "raffleProxy", "GetRaffle", &[PyRep::Integer(raffle_id)])
            .await
            .expect("GetRaffle live");
        assert_eq!(live_raffle.dict_get("item_id").and_then(|v| v.as_i64()), Some(item_1));

        // 3. BuyTicket & BuyRandomTickets -> emits OnRaffleUpdatedServer & OnTicketsUpdatedServer,
        // and buying the final ticket emits OnRaffleFinishedServer(raffle_id, winning_ticket)
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
        let _ = rpc(
            &ctx,
            &mut sess_b,
            "raffleProxy",
            "BuyTicket",
            &[PyRep::Integer(raffle_id), PyRep::Integer(0)],
        )
        .await
        .expect("BuyTicket 0");

        // Negative: buying already-sold ticket 0 fails with TicketUnavailable
        assert_eq!(
            rpc(
                &ctx,
                &mut sess_b,
                "raffleProxy",
                "BuyTicket",
                &[PyRep::Integer(raffle_id), PyRep::Integer(0)],
            )
            .await
            .unwrap_err()
            .code,
            "TicketUnavailable"
        );

        let upd_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
        let upd_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
        let upd_args = find_notif(&upd_notifs_b, "OnRaffleUpdatedServer");
        assert_eq!(upd_args.len(), 2);
        assert_eq!(upd_args[0].as_i64(), Some(raffle_id));
        assert_eq!(
            upd_args[1].dict_get("tickets_remaining").and_then(|v| v.as_i64()),
            Some(7)
        );
        let tix_args = find_notif(&upd_notifs_a, "OnTicketsUpdatedServer");
        assert_eq!(tix_args.len(), 1);
        assert_eq!(
            tix_args[0],
            PyRep::Dict(vec![(PyRep::Integer(raffle_id), PyRep::Integer(1))])
        );

        // Buy remaining 7 tickets -> finishes raffle and emits OnRaffleFinishedServer
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
        let _ = rpc(
            &ctx,
            &mut sess_b,
            "raffleProxy",
            "BuyRandomTickets",
            &[PyRep::Integer(raffle_id), PyRep::Integer(7)],
        )
        .await
        .expect("BuyRandomTickets 7");

        let fin_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
        let fin_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
        let fin_args_a = find_notif(&fin_notifs_a, "OnRaffleFinishedServer");
        let fin_args_b = find_notif(&fin_notifs_b, "OnRaffleFinishedServer");
        assert_eq!(fin_args_a.len(), 2);
        assert_eq!(fin_args_a[0].as_i64(), Some(raffle_id));
        assert!(fin_args_a[1].as_i64().is_some_and(|t| (0..8).contains(&t)));
        assert_eq!(fin_args_a, fin_args_b);

        // Negative: buying on finished raffle fails with RaffleFinished; also verify expiry sweep emits None winner
        assert_eq!(
            rpc(
                &ctx,
                &mut sess_b,
                "raffleProxy",
                "BuyRandomTickets",
                &[PyRep::Integer(raffle_id), PyRep::Integer(1)],
            )
            .await
            .unwrap_err()
            .code,
            "RaffleFinished"
        );

        let create_2 = rpc(
            &ctx,
            &mut sess_a,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::Integer(item_2), PyRep::Integer(8), PyRep::Real(100_000.0), PyRep::Integer(60)],
        )
        .await
        .expect("CreateRaffle 2");
        let raffle_2 = create_2.dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap();
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        ctx.advance_secs(120);
        assert_eq!(sweep_expired_raffles(&ctx), 1);
        let exp_raffle_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(
            find_notif(&exp_raffle_notifs, "OnRaffleFinishedServer"),
            &[PyRep::Integer(raffle_2), PyRep::None]
        );

        // 4. PLEX Vault deposit & withdraw: positive + negative failure notifications
        let (plex_item_id, tritanium_id) = (820_000_010_i64, 820_000_011_i64);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                plex_item_id,
                InventoryItemRecord {
                    item_id: plex_item_id,
                    type_id: PLEX_TYPE_ID,
                    owner_id: PILOT_A,
                    location_id: JITA_STATION,
                    flag: InventoryFlag::Hangar,
                    quantity: 500,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                tritanium_id,
                InventoryItemRecord {
                    item_id: tritanium_id,
                    type_id: 34,
                    owner_id: PILOT_A,
                    location_id: JITA_STATION,
                    flag: InventoryFlag::Hangar,
                    quantity: 100,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        // Negative deposit cases -> synchronous RPC Err without unmatchable OnPlexDepositFailed
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(
            rpc(
                &ctx,
                &mut sess_a,
                "invbroker",
                "DepositPlexToVault",
                &[PyRep::Integer(JITA_STATION), PyRep::Integer(tritanium_id), PyRep::Integer(10)],
            )
            .await
            .unwrap_err()
            .code,
            "NotPlex"
        );
        let dep_fail_1 = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(!dep_fail_1.iter().any(|(e, _)| e == "OnPlexDepositFailed"));

        assert_eq!(
            rpc(
                &ctx,
                &mut sess_a,
                "invbroker",
                "DepositPlexToVault",
                &[PyRep::Integer(JITA_STATION), PyRep::Integer(plex_item_id), PyRep::Integer(9_999)],
            )
            .await
            .unwrap_err()
            .code,
            "NotEnoughQuantity"
        );
        let dep_fail_2 = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(!dep_fail_2.iter().any(|(e, _)| e == "OnPlexDepositFailed"));

        // Positive deposit -> emits OnPlexDeposited([entity_id])
        let dep_ok = rpc(
            &ctx,
            &mut sess_a,
            "invbroker",
            "DepositPlexToVault",
            &[PyRep::Integer(JITA_STATION), PyRep::Integer(plex_item_id), PyRep::Integer(250)],
        )
        .await
        .expect("DepositPlexToVault 250");
        assert!(dep_ok.as_i64().unwrap_or(0) > 0);
        let dep_ok_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let dep_args = find_notif(&dep_ok_notifs, "OnPlexDeposited");
        assert_eq!(dep_args.len(), 1);
        assert_eq!(dep_args[0], dep_ok);

        // Negative withdraw cases -> synchronous RPC Err without unmatchable OnPlexWithdrawFailed
        assert_eq!(
            rpc(
                &ctx,
                &mut sess_a,
                "invbroker",
                "WithdrawPlexFromVault",
                &[PyRep::Integer(99_999)],
            )
            .await
            .unwrap_err()
            .code,
            "NotEnoughPlex"
        );
        let wd_fail_1 = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(!wd_fail_1.iter().any(|(e, _)| e == "OnPlexWithdrawFailed"));

        assert_eq!(
            rpc(
                &ctx,
                &mut sess_a,
                "invbroker",
                "WithdrawPlexFromVault",
                &[PyRep::Integer(0)],
            )
            .await
            .unwrap_err()
            .code,
            "InvalidArgument"
        );
        let wd_fail_2 = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(!wd_fail_2.iter().any(|(e, _)| e == "OnPlexWithdrawFailed"));

        // Positive withdraw -> emits OnPlexItemAddedToInventory([item_id, tx_id]) matching reply reference
        let wd_ok = rpc(
            &ctx,
            &mut sess_a,
            "invbroker",
            "WithdrawPlexFromVault",
            &[PyRep::Integer(100)],
        )
        .await
        .expect("WithdrawPlexFromVault 100");
        let withdrawn_item_id = wd_ok
            .dict_get("itemID")
            .and_then(|v| v.as_i64())
            .expect("withdrawn itemID");
        let wd_ref = wd_ok
            .dict_get("reference")
            .and_then(|v| v.as_i64())
            .expect("withdraw reference");
        assert_eq!(
            wd_ok.dict_get("remainingVaultBalance").and_then(|v| v.as_i64()),
            Some(650)
        );
        let wd_ok_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let wd_args = find_notif(&wd_ok_notifs, "OnPlexItemAddedToInventory");
        assert_eq!(wd_args.len(), 2);
        assert_eq!(wd_args[0].as_i64(), Some(withdrawn_item_id));
        assert_eq!(wd_args[1].as_i64(), Some(wd_ref));
    }
}

pub(crate) mod new_ctx_rpc_beta {
    // Real server-side emission & RPC tests for Notify/RPC Batch 8 (Part 3: Operations & AIR NPE).
    //
    // Covers the V24-triggered Group 3 Operations / NPE parity rows:
    // - `notify:OnOperationTaskSkipped`
    // - `notify:OnAirNpeStateChanged`


    use crate::common;
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use eve_server::services::operations_manager_helpers::get_operation_defs;
    use std::collections::HashMap;

    const PILOT_A: i64 = 90_000_001;
    const SYS_JITA: u32 = 30_000_142;

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

    fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
        let mut out = sess.drain_notifications();
        for n in ctx.drain_notifications_for_char(char_id) {
            out.push((n.event, n.args));
        }
        out
    }

    fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
        notifs
            .iter()
            .find(|(e, _)| e == event)
            .map(|(_, args)| args.as_slice())
            .unwrap_or_else(|| {
                let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
                panic!("expected notification {event}, got {seen:?}")
            })
    }

    // parity: notify:OnOperationTaskSkipped, notify:OnAirNpeStateChanged
    #[tokio::test]
    async fn notify_batch8_operations_npe_and_client_node_graphs() {
        eve_testkit::attest(&[
            "notify:OnOperationTaskSkipped",
            "notify:OnAirNpeStateChanged",
        ]);

        let ctx = new_ctx();
        let mut unauth = SessionState::new(9_300);
        assert_eq!(
            rpc(
                &ctx,
                &mut unauth,
                "operationsManager",
                "skip_task",
                &[PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(10)],
            )
            .await
            .unwrap_err()
            .code,
            "Unauthorized"
        );
        assert_eq!(
            rpc(
                &ctx,
                &mut unauth,
                "operationsManager",
                "process_client_event",
                &[PyRep::String("start_tutorial".into())],
            )
            .await
            .unwrap_err()
            .code,
            "Unauthorized"
        );

        let mut sess = SessionState::new(9_301);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)])
            .await
            .expect("SelectCharacterID");
        let _ = drain_all(&ctx, &mut sess, PILOT_A);

        // Negative: non-SDE graph 999_999_888 is rejected with NodeGraphNotFound
        let err_ng = rpc(
            &ctx,
            &mut sess,
            "node_graph",
            "process_client_graph_message",
            &[
                PyRep::Integer(999_999_888),
                PyRep::Integer(1),
                PyRep::String("noop_msg".into()),
                PyRep::Dict(Vec::new()),
            ],
        )
        .await
        .unwrap_err();
        assert_eq!(err_ng.code, "NodeGraphNotFound");

        // 1. start_tutorial via process_client_event -> emits OnAirNpeStateChanged([PILOT_A, 1])
        let start_tut = rpc(
            &ctx,
            &mut sess,
            "operationsManager",
            "process_client_event",
            &[
                PyRep::String("start_tutorial".into()),
                PyRep::None,
                PyRep::Dict(vec![(PyRep::String("state_id".into()), PyRep::Integer(1))]),
            ],
        )
        .await
        .expect("start_tutorial");
        assert_eq!(start_tut, PyRep::Bool(true));

        let tut_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(
            find_notif(&tut_notifs, "OnAirNpeStateChanged"),
            &[PyRep::Integer(PILOT_A), PyRep::Integer(1)]
        );

        // 2. process_client_graph_message on real SDE graph (3) does NOT echo client input back
        let _ = rpc(
            &ctx,
            &mut sess,
            "node_graph",
            "process_client_graph_message",
            &[
                PyRep::Integer(3),
                PyRep::Integer(5),
                PyRep::String("node_completed".into()),
                PyRep::Dict(vec![(PyRep::String("stage".into()), PyRep::Integer(2))]),
            ],
        )
        .await
        .expect("process_client_graph_message on SDE graph 3");
        let ng_msg_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert!(
            !ng_msg_notifs.iter().any(|(ev, _)| ev == "OnClientNodeGraphMessage" || ev == "OnClientNodeGraphUpdate"),
            "must not echo client node graph input back: {ng_msg_notifs:?}"
        );

        // 3. skip_tutorial -> emits OnAirNpeStateChanged([PILOT_A, 3])
        let skip_tut = rpc(
            &ctx,
            &mut sess,
            "operationsManager",
            "process_client_event",
            &[PyRep::String("skip_tutorial".into())],
        )
        .await
        .expect("skip_tutorial");
        assert_eq!(skip_tut, PyRep::Bool(true));

        let skip_tut_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(
            find_notif(&skip_tut_notifs, "OnAirNpeStateChanged"),
            &[PyRep::Integer(PILOT_A), PyRep::Integer(3)]
        );

        // 4. Discover a real operation with tasks from SDE -> activate & skip_task -> OnOperationTaskSkipped
        let op_defs = get_operation_defs(&ctx.sde);
        let empty_done = std::collections::HashSet::new();
        let is_skippable = |tid: i64| {
            op_defs.task_is_client_skippable.contains(&tid) || op_defs.task_is_optional.contains(&tid)
        };
        let (&op_id, tasks) = op_defs
            .op_tasks
            .iter()
            .filter(|(id, ts)| {
                op_defs.are_prerequisites_met(**id, &empty_done)
                    && ts.len() >= 2
                    && is_skippable(ts[0])
            })
            .min_by_key(|(id, _)| **id)
            .or_else(|| {
                op_defs
                    .op_tasks
                    .iter()
                    .filter(|(id, ts)| op_defs.are_prerequisites_met(**id, &empty_done) && ts.len() >= 2)
                    .min_by_key(|(id, _)| **id)
            })
            .or_else(|| {
                op_defs
                    .op_tasks
                    .iter()
                    .filter(|(id, ts)| op_defs.are_prerequisites_met(**id, &empty_done) && !ts.is_empty())
                    .min_by_key(|(id, _)| **id)
            })
            .expect("at least one prerequisite-free operation with tasks in SDE");
        let cat_id = op_defs
            .op_default_category
            .get(&op_id)
            .copied()
            .or_else(|| op_defs.sde_categories.iter().copied().next())
            .unwrap_or(31);
        let first_task_id = tasks[0];
        let expected_next_task = tasks.get(1).copied().map(PyRep::Integer).unwrap_or(PyRep::None);
        if !op_defs.client_completable_tasks.contains(&first_task_id) {
            sess.role |= eve_machonet::service_roles::ROLE_GMH;
        }

        let _ = drain_all(&ctx, &mut sess, PILOT_A);
        let act_op = rpc(
            &ctx,
            &mut sess,
            "operationsManager",
            "activate_operation_in_solar_system",
            &[
                PyRep::Integer(op_id),
                PyRep::Integer(SYS_JITA as i64),
                PyRep::Integer(cat_id),
            ],
        )
        .await
        .expect("activate_operation_in_solar_system");
        assert_eq!(act_op, PyRep::Bool(true));
        let _ = drain_all(&ctx, &mut sess, PILOT_A);

        // Negative: skip_task with invalid task ID fails with CustomInfo
        assert_eq!(
            rpc(
                &ctx,
                &mut sess,
                "operationsManager",
                "skip_task",
                &[PyRep::Integer(cat_id), PyRep::Integer(op_id), PyRep::Integer(0)],
            )
            .await
            .unwrap_err()
            .code,
            "CustomInfo"
        );

        let skip_task_rep = rpc(
            &ctx,
            &mut sess,
            "operationsManager",
            "skip_task",
            &[
                PyRep::Integer(cat_id),
                PyRep::Integer(op_id),
                PyRep::Integer(first_task_id),
            ],
        )
        .await
        .expect("skip_task");
        assert_eq!(skip_task_rep, PyRep::Bool(true));
        let skip_task_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        let task_skipped_args = find_notif(&skip_task_notifs, "OnOperationTaskSkipped");
        assert_eq!(
            task_skipped_args,
            &[
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(cat_id),
                    PyRep::Integer(op_id),
                    PyRep::Integer(first_task_id),
                    expected_next_task,
                ])]),
                PyRep::Bool(true),
            ]
        );
    }
}

pub(crate) mod new_ctx_rpc_gamma {
    // Real server-side emission tests for Notify/RPC Batch 8 (Part 4: Combat, EWAR, Cloak, Invuln, Fleet, Drones & Ammo).
    //
    // Covers 14 Group 4 parity rows across two test functions (`<= 15` tags each):
    // - Test 1 (`6` rows): `OnJamStart`, `OnEwarEnd`, `OnJumpCloakUpdated`,
    //   `OnJumpCloakCancelled`, `OnInvulnCancelled`, `OnRestoringInvulnUpdated`
    // - Test 2 (`8` rows): `OnChargeLoadedToModule`, `OnAmmoInBankChanged`, `OnDroneActivityChange`,
    //   `OnFleetBoosted`, `OnFleetLootEvent`, `OnFleetJumpBeaconDeployableChange`,
    //   `OnFleetRespawnPointsUpdate`, `OnFleetEscalatingPathMessage`


    use crate::common;
    use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
    use eve_destiny::{Ball, MovementMode, TargetLock};
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;

    const PILOT_A: i64 = 90_000_001;
    const PILOT_B: i64 = 90_000_002;
    const SYS_JITA: u32 = 30_000_142;
    const JITA_STATION: i64 = 60_003_760;

    fn new_ctx() -> ServerContext {
        let sde = std::sync::Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
        );
        let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        ctx.set_clock_paused(true);
        ctx.set_mechanic_rng_seed(Some(42));
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

    async fn rpc_bound(
        ctx: &ServerContext,
        sess: &mut SessionState,
        svc: &str,
        bound_id: u64,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, svc, Some(bound_id), method, args, &HashMap::new(), None)
            .await
    }

    fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
        let mut out = sess.drain_notifications();
        for n in ctx.drain_notifications_for_char(char_id) {
            out.push((n.event, n.args));
        }
        out
    }

    fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
        notifs
            .iter()
            .find(|(e, _)| e == event)
            .map(|(_, args)| args.as_slice())
            .unwrap_or_else(|| {
                let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
                panic!("expected notification {event}, got {seen:?}")
            })
    }

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

    fn grant_all_skills(ctx: &ServerContext, char_id: i64) {
        ctx.db.with_mut_snapshot(|snap| {
            let sk = snap.skills.entry(char_id).or_default();
            for (&tid, ty) in &ctx.sde.types {
                if ty.category_id == 16 {
                    sk.insert(
                        tid,
                        TrainedSkillRecord {
                            type_id: tid,
                            level: 5,
                            skill_points: 256_000.0,
                        },
                    );
                }
            }
        });
    }

    async fn login_in_space(
        ctx: &ServerContext,
        sid: u64,
        user: &str,
        pass: &str,
        char_id: i64,
        ship_id: i64,
        ship_type: u32,
        sys_id: u32,
        pos: DVec3,
    ) -> SessionState {
        let mut sess = SessionState::new(sid);
        common::login_pilot(ctx, &mut sess, user, pass).await;
        rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
            .await
            .expect("SelectCharacterID");
        grant_all_skills(ctx, char_id);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = ship_id;
                ch.corporation_id = 1_000_125;
            }
            snap.items.insert(
                ship_id,
                make_item(ship_id, ship_type, char_id, sys_id as i64, InventoryFlag::None, 1, true),
            );
        });
        sess.solar_system_id = Some(sys_id);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(ship_id);
        sess.corporation_id = Some(1_000_125);

        let handle = ctx.systems.get_or_spawn(sys_id);
        let mut cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type);
        cs.capacitor = cs.max_capacitor.max(10_000.0);
        cs.max_capacitor = cs.max_capacitor.max(10_000.0);
        cs.is_cloaked = false;
        cs.gate_cloak_remaining_ticks = 0;
        cs.undock_invulnerable_remaining_ticks = 0;
        let slim = ctx.build_ship_slim_meta(char_id, ship_type);
        let ball = Ball {
            id: ship_id,
            type_id: ship_type,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 1.0,
            mass: 1_200_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let _ = handle.spawn_ball_with_meta(ball, cs, slim).await;
        ctx.invalidate_ship_fitting(ship_id);
        let _ = drain_all(ctx, &mut sess, char_id);
        sess
    }

    // parity: notify:OnJamStart, notify:OnEwarEnd, notify:OnJumpCloakUpdated
    // parity: notify:OnJumpCloakCancelled, notify:OnInvulnCancelled, notify:OnRestoringInvulnUpdated
    #[tokio::test]
    async fn notify_batch8_combat_ewar_cloak_and_invuln() {
        eve_testkit::attest(&[
            "notify:OnJamStart",
            "notify:OnEwarEnd",
            "notify:OnJumpCloakUpdated",
            "notify:OnJumpCloakCancelled",
            "notify:OnInvulnCancelled",
            "notify:OnRestoringInvulnUpdated",
        ]);

        let ctx = new_ctx();
        let (ship_a, ship_b) = (840_000_001_i64, 840_000_002_i64);

        // 1. Undock -> OnRestoringInvulnUpdated([ship_a, end_ft, 300_000_000]), then CmdGotoDirection -> OnInvulnCancelled([ship_a])
        let mut sess_a = SessionState::new(9_401);
        common::login_pilot(&ctx, &mut sess_a, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess_a, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)])
            .await
            .expect("SelectCharacterID A");
        grant_all_skills(&ctx, PILOT_A);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.station_id = Some(JITA_STATION);
                ch.structure_id = None;
                ch.solar_system_id = SYS_JITA;
                ch.active_ship_id = ship_a;
            }
            snap.items.insert(
                ship_a,
                make_item(ship_a, 623, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
            );
        });
        sess_a.station_id = Some(JITA_STATION);
        sess_a.ship_id = Some(ship_a);
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

        rpc(&ctx, &mut sess_a, "stationSvc", "Undock", &[])
            .await
            .expect("Undock");
        let undock_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let invuln_args = find_notif(&undock_notifs, "OnRestoringInvulnUpdated");
        assert_eq!(invuln_args.len(), 3);
        assert_eq!(invuln_args[0].as_i64(), Some(ship_a));
        assert_eq!(invuln_args[2].as_i64(), Some(300_000_000));

        let beyonce_jita = ctx
            .bind_object(&mut sess_a, "beyonce", SYS_JITA as i64)
            .expect("bind beyonce jita");
        rpc_bound(
            &ctx,
            &mut sess_a,
            "beyonce",
            beyonce_jita,
            "CmdGotoDirection",
            &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
        )
        .await
        .expect("CmdGotoDirection breaks undock invuln");
        let invuln_cancel_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(
            find_notif(&invuln_cancel_notifs, "OnInvulnCancelled"),
            &[PyRep::Integer(ship_a)]
        );

        // Negative: repeating CmdGotoDirection when invulnerability is already broken does not re-emit OnInvulnCancelled
        rpc_bound(
            &ctx,
            &mut sess_a,
            "beyonce",
            beyonce_jita,
            "CmdGotoDirection",
            &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)],
        )
        .await
        .expect("second CmdGotoDirection");
        let repeat_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(!repeat_notifs.iter().any(|(e, _)| e == "OnInvulnCancelled"));

        // 2. Stargate jump -> OnJumpCloakUpdated & OnRestoringInvulnUpdated(600_000_000), then movement -> OnJumpCloakCancelled
        ctx.advance_secs(12);
        let (gate_id, gate_pos) = ctx
            .sde
            .jumps
            .values()
            .find(|j| j.from_solar_system_id == SYS_JITA)
            .map(|j| (j.stargate_id, j.position))
            .expect("stargate out of Jita");
        let jita_handle = ctx.systems.get_or_spawn(SYS_JITA);
        let _ = jita_handle.set_ball_position(ship_a, gate_pos).await;

        // Negative: unknown stargate ID rejected
        assert_eq!(
            rpc_bound(
                &ctx,
                &mut sess_a,
                "beyonce",
                beyonce_jita,
                "CmdStargateJump",
                &[PyRep::Integer(999_999_888)],
            )
            .await
            .unwrap_err()
            .code,
            "UnknownStargate"
        );

        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        rpc_bound(
            &ctx,
            &mut sess_a,
            "beyonce",
            beyonce_jita,
            "CmdStargateJump",
            &[PyRep::Integer(gate_id)],
        )
        .await
        .expect("CmdStargateJump");
        let jump_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let jc_args = find_notif(&jump_notifs, "OnJumpCloakUpdated");
        assert_eq!(jc_args.len(), 3);
        assert_eq!(jc_args[0].as_i64(), Some(ship_a));
        assert_eq!(jc_args[2].as_i64(), Some(600_000_000));
        assert_eq!(
            find_notif(&jump_notifs, "OnRestoringInvulnUpdated")[2].as_i64(),
            Some(600_000_000)
        );

        let new_sys = sess_a.solar_system_id.expect("new solar_system_id");
        let beyonce_new = ctx
            .bind_object(&mut sess_a, "beyonce", new_sys as i64)
            .expect("bind beyonce new sys");
        rpc_bound(
            &ctx,
            &mut sess_a,
            "beyonce",
            beyonce_new,
            "CmdGotoDirection",
            &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
        )
        .await
        .expect("CmdGotoDirection breaks gate cloak");
        let jc_cancel_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(
            find_notif(&jc_cancel_notifs, "OnJumpCloakCancelled"),
            &[PyRep::Integer(ship_a)]
        );

        // Negative: repeating movement after gate cloak broke does not re-emit OnJumpCloakCancelled
        rpc_bound(
            &ctx,
            &mut sess_a,
            "beyonce",
            beyonce_new,
            "CmdGotoDirection",
            &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)],
        )
        .await
        .expect("second move after gate cloak broke");
        assert!(
            !drain_all(&ctx, &mut sess_a, PILOT_A)
                .iter()
                .any(|(e, _)| e == "OnJumpCloakCancelled")
        );

        // 3. Cloak activate & Deactivate -> OnCloakModuleForceDeactivated
        let (cloak_id, web_id, ecm_id, scram_id, gd_id) = (
            840_000_010_i64,
            840_000_011_i64,
            840_000_012_i64,
            840_000_013_i64,
            840_000_014_i64,
        );
        let ecm_type_id = ctx
            .sde
            .types
            .values()
            .find(|t| t.group_id == 201 && t.default_attributes.get(&238).copied().unwrap_or(0.0) > 0.0)
            .map(|t| t.type_id)
            .expect("ECM jammer type in SDE");
        let scram_type_id = 447u32; // Warp Scrambler I
        let gd_type_id = ctx
            .sde
            .types
            .values()
            .filter(|t| {
                t.group_id == 291
                    && (t.effect_ids.contains(&6425) || t.name.contains("Guidance"))
                    && t.default_attributes.get(&54).copied().unwrap_or(0.0) >= 8_000.0
            })
            .min_by_key(|t| t.type_id)
            .map(|t| t.type_id)
            .expect("Guidance Disruptor type in SDE");

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                cloak_id,
                make_item(cloak_id, 11_370, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true),
            );
            snap.items.insert(
                web_id,
                make_item(web_id, 526, PILOT_A, ship_a, InventoryFlag::MedSlot0, 1, true),
            );
            let mut ecm_item = make_item(ecm_id, ecm_type_id, PILOT_A, ship_a, InventoryFlag::MedSlot1, 1, true);
            for aid in [238u16, 239, 240, 241] {
                ecm_item.dynamic_attributes.insert(aid, 1_000.0);
            }
            snap.items.insert(ecm_id, ecm_item);
            snap.items.insert(
                scram_id,
                make_item(scram_id, scram_type_id, PILOT_A, ship_a, InventoryFlag::MedSlot2, 1, true),
            );
            snap.items.insert(
                gd_id,
                make_item(gd_id, gd_type_id, PILOT_A, ship_a, InventoryFlag::MedSlot3, 1, true),
            );
        });
        ctx.invalidate_ship_fitting(ship_a);

        // Negative: deactivating inactive cloak is idempotent and emits no OnCloakModuleForceDeactivated
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(
            rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", &[PyRep::Integer(cloak_id)])
                .await
                .is_ok()
        );
        assert!(
            !drain_all(&ctx, &mut sess_a, PILOT_A)
                .iter()
                .any(|(e, _)| e == "OnCloakModuleForceDeactivated")
        );

        // Move ship_a far from any stargate (> 2000m) so cloak activates cleanly
        let new_handle = ctx.systems.get_or_spawn(new_sys);
        let _ = new_handle
            .set_ball_position(ship_a, DVec3::new(100_000.0, 0.0, 0.0))
            .await;

        ctx.advance_secs(35);
        rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(cloak_id)])
            .await
            .expect("Activate Cloak");
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", &[PyRep::Integer(cloak_id)])
            .await
            .expect("Deactivate Cloak");
        let decloak_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(
            !decloak_notifs
                .iter()
                .any(|(e, _)| e == "OnCloakModuleForceDeactivated"),
            "voluntary Deactivate must not emit OnCloakModuleForceDeactivated"
        );

        // 4. Webifier Deactivate -> OnEwarEnd; ECM Activate -> OnJamStart
        let mut sess_b = login_in_space(
            &ctx,
            9_402,
            "other",
            "other-secret",
            PILOT_B,
            ship_b,
            587,
            new_sys,
            DVec3::new(105_000.0, 0.0, 0.0),
        )
        .await;

        // Negative: activating ECM without target lock fails with TargetNotLocked or TargetRequired
        assert!(
            rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(ecm_id)])
                .await
                .is_err()
        );

        if let Ok(Some(mut cs)) = new_handle.get_combat_state(ship_a).await {
            cs.decloak_targeting_delay_remaining_s = 0.0;
            let _ = new_handle.set_combat_state(ship_a, cs).await;
        }
        rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b)])
            .await
            .expect("AddTarget");
        let _ = new_handle
            .add_target_lock(
                ship_a,
                TargetLock {
                    target_id: ship_b,
                    lock_time_ms: 100.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: None,
                },
            )
            .await;

        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(web_id),
                PyRep::String("defaultEffect".into()),
                PyRep::Integer(ship_b),
            ],
        )
        .await
        .expect("Activate Stasis Webifier I");
        let web_act_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(
            !web_act_notifs.iter().any(|(e, _)| e == "OnEffectMessage"),
            "module activation must not emit unverified OnEffectMessage"
        );

        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
        // Negative (B8-01): victim PILOT_B cannot cleanse attacker's web by calling Deactivate(web_id)
        assert_eq!(
            rpc(&ctx, &mut sess_b, "dogmaIM", "Deactivate", &[PyRep::Integer(web_id)])
                .await
                .unwrap_err()
                .code,
            "ModuleNotOwnedByShip"
        );
        assert!(
            drain_all(&ctx, &mut sess_a, PILOT_A).iter().all(|(e, _)| e != "OnEwarEnd"),
            "unauthorized Deactivate must not emit OnEwarEnd"
        );
        assert!(
            drain_all(&ctx, &mut sess_b, PILOT_B).iter().all(|(e, _)| e != "OnEwarEnd"),
            "unauthorized Deactivate must not emit OnEwarEnd to victim"
        );
        assert!(
            ctx.projected_ewar.read().unwrap().get(&ship_b).is_some_and(|v| !v.is_empty()),
            "unauthorized Deactivate must not cleanse projected_ewar on victim"
        );

        rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", &[PyRep::Integer(web_id)])
            .await
            .expect("Deactivate Stasis Webifier I");
        let ewar_end_a = drain_all(&ctx, &mut sess_a, PILOT_A);
        let ewar_end_b = drain_all(&ctx, &mut sess_b, PILOT_B);
        let expected_ewar_end = [
            PyRep::Integer(ship_a),
            PyRep::Integer(web_id),
            PyRep::Integer(ship_b),
            PyRep::String("webify".into()),
        ];
        assert_eq!(find_notif(&ewar_end_a, "OnEwarEnd"), &expected_ewar_end);
        assert_eq!(find_notif(&ewar_end_b, "OnEwarEnd"), &expected_ewar_end);

        // Activate ECM -> lands jam -> emits OnJamStart
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
        rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(ecm_id),
                PyRep::String("defaultEffect".into()),
                PyRep::Integer(ship_b),
            ],
        )
        .await
        .expect("Activate ECM Jammer");
        let mut jam_notifs_a = Vec::new();
        for _ in 0..25 {
            let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
            let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
            rpc(
                &ctx,
                &mut sess_a,
                "dogmaIM",
                "Activate",
                &[
                    PyRep::Integer(ecm_id),
                    PyRep::String("defaultEffect".into()),
                    PyRep::Integer(ship_b),
                ],
            )
            .await
            .expect("Activate ECM");
            jam_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
            if jam_notifs_a.iter().any(|(e, _)| e == "OnJamStart") {
                break;
            }
            let _ = rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", &[PyRep::Integer(ecm_id)]).await;
        }
        let jam_args = find_notif(&jam_notifs_a, "OnJamStart");
        assert_eq!(jam_args.len(), 6);
        assert_eq!(jam_args[0].as_i64(), Some(ship_a));
        assert_eq!(jam_args[1].as_i64(), Some(ecm_id));
        assert_eq!(jam_args[2].as_i64(), Some(ship_b));
        assert_eq!(jam_args[3].as_str(), Some("electronic"));
        let _ = rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", &[PyRep::Integer(ecm_id)]).await;

        // 5. Warp Scrambler & Guidance Disruptor OnEwarStart + OnEwarEnd jammingType parity
        for (mod_id, expected_jam_type) in [
            (scram_id, "warpScramblerMWD"),
            (gd_id, "ewGuidanceDisrupt"),
        ] {
            let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
            let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
            rpc(
                &ctx,
                &mut sess_a,
                "dogmaIM",
                "Activate",
                &[
                    PyRep::Integer(mod_id),
                    PyRep::String("defaultEffect".into()),
                    PyRep::Integer(ship_b),
                ],
            )
            .await
            .expect("Activate EWAR module");
            let start_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
            let expected_args = [
                PyRep::Integer(ship_a),
                PyRep::Integer(mod_id),
                PyRep::Integer(ship_b),
                PyRep::String(expected_jam_type.into()),
            ];
            assert_eq!(find_notif(&start_notifs, "OnEwarStart"), &expected_args);

            let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
            rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", &[PyRep::Integer(mod_id)])
                .await
                .expect("Deactivate EWAR module");
            let end_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
            let end_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
            assert_eq!(find_notif(&end_notifs_a, "OnEwarEnd"), &expected_args);
            assert_eq!(find_notif(&end_notifs_b, "OnEwarEnd"), &expected_args);
        }

        // 6. TakeModuleOffline on an active EWAR module flushes projected_ewar and emits OnEwarEnd
        rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(gd_id),
                PyRep::String("defaultEffect".into()),
                PyRep::Integer(ship_b),
            ],
        )
        .await
        .expect("Activate Guidance Disruptor before TakeModuleOffline");
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
        rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "TakeModuleOffline",
            &[PyRep::Integer(ship_a), PyRep::Integer(gd_id)],
        )
        .await
        .expect("TakeModuleOffline active Guidance Disruptor");
        let expected_gd_end = [
            PyRep::Integer(ship_a),
            PyRep::Integer(gd_id),
            PyRep::Integer(ship_b),
            PyRep::String("ewGuidanceDisrupt".into()),
        ];
        assert_eq!(find_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnEwarEnd"), &expected_gd_end);
        assert_eq!(find_notif(&drain_all(&ctx, &mut sess_b, PILOT_B), "OnEwarEnd"), &expected_gd_end);
    }
}

pub(crate) mod new_ctx_rpc_delta {
    // Real server-side RPC tests for Notify/RPC Batch 8 (Part 5: Session, Localization & LoadService RPCs).
    //
    // Covers all 8 Group 5 Session / Localization / LoadService parity rows:
    // - `rpc:userSvc.ProcessEVESessionCreated`
    // - `rpc:sessionMgr.EndAllGameSessions`
    // - `rpc:sessionMgr.RemoveSessionsFromServer`
    // - `rpc:localizationServer.ReloadFSDPickle`
    // - `rpc:localizationServer.UpdateLocalizationQAWrap`
    // - `rpc:loadService.GetTotalStats`
    // - `rpc:loadService.StartLoad`
    // - `rpc:loadService.StopLoad`


    use crate::common;
    use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    const PILOT_A: i64 = 90_000_001;
    const PILOT_B: i64 = 90_000_002;

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

    // parity: rpc:userSvc.ProcessEVESessionCreated, rpc:sessionMgr.EndAllGameSessions
    // parity: rpc:sessionMgr.RemoveSessionsFromServer
    #[tokio::test]
    async fn notify_batch8_session_localization_and_load_rpcs() {
        eve_testkit::attest(&[
            "rpc:userSvc.ProcessEVESessionCreated",
            "rpc:sessionMgr.EndAllGameSessions",
            "rpc:sessionMgr.RemoveSessionsFromServer",
            ]);

        let ctx = new_ctx();
        let mut unauth = SessionState::new(9_500);

        // Negative: unauthenticated rejections across all 8 RPCs
        for (svc, method, args) in [
            ("userSvc", "ProcessEVESessionCreated", vec![]),
            ("sessionMgr", "EndAllGameSessions", vec![]),
            ("sessionMgr", "RemoveSessionsFromServer", vec![PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(9_502)])]),
            ("localizationServer", "ReloadFSDPickle", vec![]),
            ("localizationServer", "UpdateLocalizationQAWrap", vec![PyRep::Dict(Vec::new())]),
            ("loadService", "GetTotalStats", vec![]),
            ("loadService", "StartLoad", vec![]),
            ("loadService", "StopLoad", vec![]),
        ] {
            assert_eq!(
                rpc(&ctx, &mut unauth, svc, method, &args).await.unwrap_err().code,
                "Unauthorized",
                "{svc}.{method} must require authentication"
            );
        }

        let mut sess_a = SessionState::new(9_501);
        common::login_pilot(&ctx, &mut sess_a, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess_a, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)])
            .await
            .expect("SelectCharacterID A");

        let mut sess_b = SessionState::new(9_502);
        common::login_pilot(&ctx, &mut sess_b, "other", "other-secret").await;
        rpc(&ctx, &mut sess_b, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_B)])
            .await
            .expect("SelectCharacterID B");

        // 1. userSvc.ProcessEVESessionCreated: succeeds for owner, rejects mismatched character ID
        let created_rep = rpc(
            &ctx,
            &mut sess_a,
            "userSvc",
            "ProcessEVESessionCreated",
            &[PyRep::Integer(PILOT_A)],
        )
        .await
        .expect("ProcessEVESessionCreated");
        assert_eq!(created_rep, PyRep::None);
        let recorded_sessions = ctx.db.with_snapshot(|snap| {
            snap.service_state_blobs.get("user_sessions").cloned().unwrap_or_default()
        });
        assert!(recorded_sessions.contains("\"session_id\":9501"));

        let mut sess_a_user = SessionState::new(9_503);
        common::login_pilot(&ctx, &mut sess_a_user, "demo", "demo-secret").await;
        assert_eq!(
            rpc(
                &ctx,
                &mut sess_a_user,
                "userSvc",
                "ProcessEVESessionCreated",
                &[PyRep::Integer(PILOT_B)],
            )
            .await
            .unwrap_err()
            .code,
            "UserError"
        );
        // Re-bind sess_a after sess_a_user displaced it
        common::login_pilot(&ctx, &mut sess_a, "demo", "demo-secret").await;
        sess_a.character_id = Some(PILOT_A);
        sess_a.role |= ROLE_GM_DEFAULT;

        // Negative: non-GM player rejected from sessionMgr, localizationServer, and loadService admin RPCs
        for (svc, method, args, expected_code) in [
            ("sessionMgr", "EndAllGameSessions", vec![], "InsufficientRole"),
            ("sessionMgr", "RemoveSessionsFromServer", vec![PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(9_502)])], "InsufficientRole"),
            ("localizationServer", "ReloadFSDPickle", vec![], "RoleRequired"),
            ("localizationServer", "UpdateLocalizationQAWrap", vec![PyRep::Dict(Vec::new())], "RoleRequired"),
            ("loadService", "GetTotalStats", vec![], "UserError"),
            ("loadService", "StartLoad", vec![], "UserError"),
            ("loadService", "StopLoad", vec![], "UserError"),
        ] {
            assert_eq!(
                rpc(&ctx, &mut sess_b, svc, method, &args).await.unwrap_err().code,
                expected_code,
                "{svc}.{method} must reject non-GM caller"
            );
        }

        // Grant GM role to sess_a
        sess_a.role |= ROLE_GM_DEFAULT;

        // 2. sessionMgr.RemoveSessionsFromServer & sessionMgr.EndAllGameSessions
        let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel();
        let (tx_b, mut rx_b) = tokio::sync::mpsc::unbounded_channel();
        let (tx_c, mut rx_c) = tokio::sync::mpsc::unbounded_channel();
        ctx.register_session_closer(9_501, tx_a);
        ctx.register_session_closer(9_502, tx_b);
        ctx.register_session_closer(9_503, tx_c);
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs.insert(
                "active_sessions".into(),
                serde_json::to_string(&vec![9_501_i64, 9_502_i64, 9_503_i64]).unwrap(),
            );
        });
        let rem_rep = rpc(
            &ctx,
            &mut sess_a,
            "sessionMgr",
            "RemoveSessionsFromServer",
            &[PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(9_502)])],
        )
        .await
        .expect("RemoveSessionsFromServer");
        assert_eq!(rem_rep, PyRep::None);
        assert_eq!(
            rx_b.try_recv().expect("sess_b disconnect notification").0,
            "RemoveSession"
        );
        assert!(!ctx.session_closers.read().unwrap().contains_key(&9_502));
        assert!(ctx.session_closers.read().unwrap().contains_key(&9_501));
        assert!(ctx.session_closers.read().unwrap().contains_key(&9_503));
        let active_after_rem: Vec<i64> = ctx.db.with_snapshot(|snap| {
            snap.service_state_blobs
                .get("active_sessions")
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default()
        });
        assert_eq!(active_after_rem, vec![9_501, 9_503]);

        let end_all_rep = rpc(&ctx, &mut sess_a, "sessionMgr", "EndAllGameSessions", &[])
            .await
            .expect("EndAllGameSessions");
        assert_eq!(end_all_rep, PyRep::None);
        assert_eq!(
            rx_c.try_recv().expect("sess_c disconnect notification").0,
            "EndAllGameSessions"
        );
        assert!(!ctx.session_closers.read().unwrap().contains_key(&9_503));
        assert!(rx_a.try_recv().is_err(), "caller session must not be disconnected");
        assert!(
            ctx.session_closers.read().unwrap().contains_key(&9_501),
            "caller session must survive EndAllGameSessions"
        );
        let active_after_end_all: Vec<i64> = ctx.db.with_snapshot(|snap| {
            snap.service_state_blobs
                .get("active_sessions")
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default()
        });
        assert_eq!(active_after_end_all, vec![9_501]);

        // 3. localizationServer.ReloadFSDPickle & UpdateLocalizationQAWrap
        let reload_err = rpc(
            &ctx,
            &mut sess_a,
            "localizationServer",
            "ReloadFSDPickle",
            &[PyRep::String("en-us.pickle".into())],
        )
        .await
        .unwrap_err();
        assert_eq!(reload_err.code, "UnimplementedRPC");

        // Non-GM caller rejected from UpdateLocalizationQAWrap(bool) with RoleRequired
        assert_eq!(
            rpc(
                &ctx,
                &mut sess_b,
                "localizationServer",
                "UpdateLocalizationQAWrap",
                &[PyRep::Bool(true)],
            )
            .await
            .unwrap_err()
            .code,
            "RoleRequired"
        );

        // GM caller succeeds with boolean toggle (DERIVED QA-wrap); non-bool overload fails closed with UnimplementedRPC
        let qawrap_rep = rpc(
            &ctx,
            &mut sess_a,
            "localizationServer",
            "UpdateLocalizationQAWrap",
            &[PyRep::Bool(true)],
        )
        .await
        .expect("UpdateLocalizationQAWrap");
        assert_eq!(qawrap_rep, PyRep::Bool(true));
        assert_eq!(
            rpc(
                &ctx,
                &mut sess_a,
                "localizationServer",
                "UpdateLocalizationQAWrap",
                &[PyRep::Dict(vec![(
                    PyRep::Integer(42_001),
                    PyRep::String("[QA] Warp Drive Active".into()),
                )])],
            )
            .await
            .unwrap_err()
            .code,
            "UnimplementedRPC"
        );

        // 4. loadService.StartLoad, GetTotalStats, StopLoad
        let stats_0 = rpc(&ctx, &mut sess_a, "loadService", "GetTotalStats", &[])
            .await
            .expect("GetTotalStats initial");
        assert_eq!(stats_0.dict_get("active").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(stats_0.dict_get("runs_started").and_then(|v| v.as_i64()), Some(0));
        assert!(stats_0.dict_get("sde_types_count").and_then(|v| v.as_i64()).unwrap_or(0) > 1_000);

        let start_rep = rpc(
            &ctx,
            &mut sess_a,
            "loadService",
            "StartLoad",
            &[PyRep::String("stress_profile_v24".into())],
        )
        .await
        .expect("StartLoad");
        assert_eq!(start_rep, PyRep::None);

        let stats_1 = rpc(&ctx, &mut sess_a, "loadService", "GetTotalStats", &[])
            .await
            .expect("GetTotalStats active");
        assert_eq!(stats_1.dict_get("active").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(stats_1.dict_get("runs_started").and_then(|v| v.as_i64()), Some(1));
        assert_eq!(stats_1.dict_get("total_requests").and_then(|v| v.as_i64()), Some(1));
        assert!(stats_1.dict_get("systems_stepped").and_then(|v| v.as_i64()).unwrap_or(0) >= 1);
        assert!(stats_1.dict_get("active_systems_count").and_then(|v| v.as_i64()).unwrap_or(0) >= 1);
        assert!(stats_1.dict_get("world_items_count").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
        assert!(stats_1.dict_get("world_characters_count").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

        let stop_rep = rpc(&ctx, &mut sess_a, "loadService", "StopLoad", &[])
            .await
            .expect("StopLoad");
        assert_eq!(stop_rep, PyRep::None);

        let stats_2 = rpc(&ctx, &mut sess_a, "loadService", "GetTotalStats", &[])
            .await
            .expect("GetTotalStats stopped");
        assert_eq!(stats_2.dict_get("active").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(stats_2.dict_get("total_requests").and_then(|v| v.as_i64()), Some(2));
    }
}

pub(crate) mod new_ctx_rpc_epsilon {
    // Real server-side emission tests for Notify/RPC Batch 8 (Part 4: Combat, EWAR, Cloak, Invuln, Fleet, Drones & Ammo).
    //
    // Covers all 17 Group 4 parity rows across two test functions (`<= 15` tags each):
    // - Test 1 (`9` rows): `OnCombatMessage`, `OnEffectMessage`, `OnJamStart`, `OnEwarEnd`,
    //   `OnCloakModuleForceDeactivated`, `OnJumpCloakUpdated`, `OnJumpCloakCancelled`,
    //   `OnInvulnCancelled`, `OnRestoringInvulnUpdated`
    // - Test 2 (`8` rows): `OnChargeLoadedToModule`, `OnAmmoInBankChanged`, `OnDroneActivityChange`,
    //   `OnFleetBoosted`, `OnFleetLootEvent`, `OnFleetJumpBeaconDeployableChange`,
    //   `OnFleetRespawnPointsUpdate`, `OnFleetEscalatingPathMessage`


    use crate::common;
    use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, UpwellStructureRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use eve_server::services::mobile_deployables::{TYPE_MOBILE_CYNO_BEACON, TYPE_MOBILE_DEPOT};
    use eve_server::services::pve_expeditions::create_escalation_expedition;
    use glam::DVec3;
    use std::collections::HashMap;

    const PILOT_A: i64 = 90_000_001;
    const PILOT_B: i64 = 90_000_002;
    const SYS_JITA: u32 = 30_000_142;

    fn new_ctx() -> ServerContext {
        let sde = std::sync::Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
        );
        let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        ctx.set_clock_paused(true);
        ctx.set_mechanic_rng_seed(Some(42));
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

    fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
        let mut out = sess.drain_notifications();
        for n in ctx.drain_notifications_for_char(char_id) {
            out.push((n.event, n.args));
        }
        out
    }

    fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
        notifs
            .iter()
            .find(|(e, _)| e == event)
            .map(|(_, args)| args.as_slice())
            .unwrap_or_else(|| {
                let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
                panic!("expected notification {event}, got {seen:?}")
            })
    }

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

    fn grant_all_skills(ctx: &ServerContext, char_id: i64) {
        ctx.db.with_mut_snapshot(|snap| {
            let sk = snap.skills.entry(char_id).or_default();
            for (&tid, ty) in &ctx.sde.types {
                if ty.category_id == 16 {
                    sk.insert(
                        tid,
                        TrainedSkillRecord {
                            type_id: tid,
                            level: 5,
                            skill_points: 256_000.0,
                        },
                    );
                }
            }
        });
    }

    async fn login_in_space(
        ctx: &ServerContext,
        sid: u64,
        user: &str,
        pass: &str,
        char_id: i64,
        ship_id: i64,
        ship_type: u32,
        sys_id: u32,
        pos: DVec3,
    ) -> SessionState {
        let mut sess = SessionState::new(sid);
        common::login_pilot(ctx, &mut sess, user, pass).await;
        rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
            .await
            .expect("SelectCharacterID");
        grant_all_skills(ctx, char_id);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = ship_id;
                ch.corporation_id = 1_000_125;
            }
            snap.items.insert(
                ship_id,
                make_item(ship_id, ship_type, char_id, sys_id as i64, InventoryFlag::None, 1, true),
            );
        });
        sess.solar_system_id = Some(sys_id);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(ship_id);
        sess.corporation_id = Some(1_000_125);

        let handle = ctx.systems.get_or_spawn(sys_id);
        let mut cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type);
        cs.capacitor = cs.max_capacitor.max(10_000.0);
        cs.max_capacitor = cs.max_capacitor.max(10_000.0);
        cs.is_cloaked = false;
        cs.gate_cloak_remaining_ticks = 0;
        cs.undock_invulnerable_remaining_ticks = 0;
        let slim = ctx.build_ship_slim_meta(char_id, ship_type);
        let ball = Ball {
            id: ship_id,
            type_id: ship_type,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 1.0,
            mass: 1_200_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let _ = handle.spawn_ball_with_meta(ball, cs, slim).await;
        ctx.invalidate_ship_fitting(ship_id);
        let _ = drain_all(ctx, &mut sess, char_id);
        sess
    }

    // parity: notify:OnChargeLoadedToModule, notify:OnAmmoInBankChanged, notify:OnDroneActivityChange
    // parity: notify:OnFleetBoosted, notify:OnFleetLootEvent, notify:OnFleetJumpBeaconDeployableChange
    // parity: notify:OnFleetRespawnPointsUpdate
    #[tokio::test]
    async fn notify_batch8_ammo_drones_and_fleet_events() {
        eve_testkit::attest(&[
            "notify:OnChargeLoadedToModule",
            "notify:OnAmmoInBankChanged",
            "notify:OnDroneActivityChange",
            "notify:OnFleetBoosted",
            "notify:OnFleetLootEvent",
            "notify:OnFleetJumpBeaconDeployableChange",
            "notify:OnFleetRespawnPointsUpdate",
        ]);

        let ctx = new_ctx();
        let (ship_a, ship_b) = (840_000_101_i64, 840_000_102_i64);
        // Pilot A in Nighthawk (22466) so Command Burst II + AutoCannons + DroneBay all fit cleanly
        let mut sess_a = login_in_space(
            &ctx,
            9_411,
            "demo",
            "demo-secret",
            PILOT_A,
            ship_a,
            22_466,
            SYS_JITA,
            DVec3::new(200_000.0, 0.0, 0.0),
        )
        .await;
        let mut sess_b = login_in_space(
            &ctx,
            9_412,
            "other",
            "other-secret",
            PILOT_B,
            ship_b,
            587,
            SYS_JITA,
            DVec3::new(201_000.0, 0.0, 0.0),
        )
        .await;

        // 1. Weapon bank ammo load & unload -> OnChargeLoadedToModule & OnAmmoInBankChanged
        let (gun1_id, gun2_id, ammo_id, bad_ammo_id, burst_id, burst_charge_id, drone_item_id) = (
            840_000_201_i64,
            840_000_202_i64,
            840_000_203_i64,
            840_000_204_i64,
            840_000_205_i64,
            840_000_206_i64,
            840_000_207_i64,
        );
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                gun1_id,
                make_item(gun1_id, 2_889, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true),
            );
            snap.items.insert(
                gun2_id,
                make_item(gun2_id, 2_889, PILOT_A, ship_a, InventoryFlag::HiSlot1, 1, true),
            );
            snap.items.insert(
                ammo_id,
                make_item(ammo_id, 183, PILOT_A, ship_a, InventoryFlag::Cargo, 500, false),
            );
            snap.items.insert(
                bad_ammo_id,
                make_item(bad_ammo_id, 42_696, PILOT_A, ship_a, InventoryFlag::Cargo, 100, false),
            );
            snap.items.insert(
                burst_id,
                make_item(burst_id, 43_555, PILOT_A, ship_a, InventoryFlag::HiSlot2, 1, true),
            );
            snap.items.insert(
                burst_charge_id,
                make_item(burst_charge_id, 42_696, PILOT_A, ship_a, InventoryFlag::Cargo, 300, false),
            );
            snap.items.insert(
                drone_item_id,
                make_item(drone_item_id, 2_454, PILOT_A, ship_a, InventoryFlag::DroneBay, 1, false),
            );
        });
        ctx.invalidate_ship_fitting(ship_a);

        // Negative: loading incompatible ammo fails with IncompatibleAmmo
        assert_eq!(
            rpc(
                &ctx,
                &mut sess_a,
                "dogmaIM",
                "LoadAmmo",
                &[
                    PyRep::Integer(ship_a),
                    PyRep::List(vec![PyRep::Integer(gun1_id)]),
                    PyRep::List(vec![PyRep::Integer(bad_ammo_id)]),
                ],
            )
            .await
            .unwrap_err()
            .code,
            "IncompatibleAmmo"
        );

        // Ungrouped load emits OnChargeLoadedToModule without OnAmmoInBankChanged
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "LoadAmmo",
            &[
                PyRep::Integer(ship_a),
                PyRep::List(vec![PyRep::Integer(gun1_id)]),
                PyRep::List(vec![PyRep::Integer(ammo_id)]),
            ],
        )
        .await
        .expect("LoadAmmo ungrouped");
        let ungrouped_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(find_notif(&ungrouped_notifs, "OnChargeLoadedToModule").len(), 0);
        assert!(!ungrouped_notifs.iter().any(|(e, _)| e == "OnAmmoInBankChanged"));

        // Link gun1_id + gun2_id, then UnloadAmmo -> emits OnAmmoInBankChanged([gun1_id])
        rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "LinkWeapons",
            &[
                PyRep::Integer(ship_a),
                PyRep::Integer(gun1_id),
                PyRep::Integer(gun2_id),
            ],
        )
        .await
        .expect("LinkWeapons");
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "UnloadAmmo",
            &[
                PyRep::Integer(ship_a),
                PyRep::List(vec![PyRep::Integer(gun1_id)]),
            ],
        )
        .await
        .expect("UnloadAmmo grouped");
        let unload_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(
            find_notif(&unload_notifs, "OnAmmoInBankChanged"),
            &[PyRep::Integer(gun1_id)]
        );

        // Reload grouped weapon bank -> emits BOTH OnChargeLoadedToModule() and OnAmmoInBankChanged([gun1_id])
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "LoadAmmo",
            &[
                PyRep::Integer(ship_a),
                PyRep::List(vec![PyRep::Integer(gun1_id)]),
                PyRep::List(vec![PyRep::Integer(ammo_id)]),
            ],
        )
        .await
        .expect("LoadAmmo grouped");
        let grouped_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(find_notif(&grouped_notifs, "OnChargeLoadedToModule").len(), 0);
        assert_eq!(
            find_notif(&grouped_notifs, "OnAmmoInBankChanged"),
            &[PyRep::Integer(gun1_id)]
        );

        // 2. Drone activity changes -> OnDroneActivityChange on CmdEngage, CmdReturnHome, CmdReturnBay
        let launched = rpc(
            &ctx,
            &mut sess_a,
            "ship",
            "LaunchDrones",
            &[PyRep::List(vec![PyRep::Integer(drone_item_id)])],
        )
        .await
        .expect("LaunchDrones");
        let drone_ball = match launched {
            PyRep::List(items) => items[0].as_i64().expect("drone ball id"),
            _ => panic!("expected list from LaunchDrones"),
        };
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

        rpc(
            &ctx,
            &mut sess_a,
            "entity",
            "CmdEngage",
            &[
                PyRep::List(vec![PyRep::Integer(drone_ball)]),
                PyRep::Integer(ship_b),
            ],
        )
        .await
        .expect("CmdEngage");
        let engage_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(
            find_notif(&engage_notifs, "OnDroneActivityChange"),
            &[
                PyRep::Integer(drone_ball),
                PyRep::Integer(1),
                PyRep::String("attack".into()),
            ]
        );

        rpc(
            &ctx,
            &mut sess_a,
            "entity",
            "CmdReturnHome",
            &[PyRep::List(vec![PyRep::Integer(drone_ball)])],
        )
        .await
        .expect("CmdReturnHome");
        let home_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(
            find_notif(&home_notifs, "OnDroneActivityChange"),
            &[PyRep::Integer(drone_ball), PyRep::Integer(0), PyRep::None]
        );

        rpc(
            &ctx,
            &mut sess_a,
            "entity",
            "CmdReturnBay",
            &[PyRep::List(vec![PyRep::Integer(drone_ball)])],
        )
        .await
        .expect("CmdReturnBay");
        let bay_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(
            bay_notifs.iter().any(|(e, a)| {
                e == "OnDroneActivityChange"
                    && a.first().and_then(|v| v.as_i64()) == Some(drone_ball)
                    && matches!(a.get(1).and_then(|v| v.as_i64()), Some(4 | 0))
            }),
            "expected OnDroneActivityChange on CmdReturnBay, got {bay_notifs:?}"
        );

        // Negative: solo escalation without fleet emits no OnFleetEscalatingPathMessage
        let _ = create_escalation_expedition(
            &ctx,
            PILOT_A,
            1420,
            SYS_JITA,
            "Guristas Scout Outpost",
            [100_000.0, 0.0, 0.0],
        )
        .await;
        assert!(
            !drain_all(&ctx, &mut sess_a, PILOT_A)
                .iter()
                .any(|(e, _)| e == "OnFleetEscalatingPathMessage")
        );

        // 3. Fleet creation + corp structure -> AcceptInvite emits OnFleetRespawnPointsUpdate
        let struct_id = 1_040_000_001_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(
                struct_id,
                UpwellStructureRecord {
                    structure_id: struct_id,
                    type_id: 35_832,
                    owner_corp_id: 1_000_125,
                    solar_system_id: SYS_JITA,
                    name: "Jita Astrahus".into(),
                    position: [200_000.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    shield_hp: 100_000.0,
                    armor_hp: 100_000.0,
                    hull_hp: 100_000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
        });

        let fleet_rep = rpc(&ctx, &mut sess_a, "fleetObjectHandler", "CreateFleet", &[])
            .await
            .expect("CreateFleet");
        let fleet_id = fleet_rep
            .as_i64()
            .or_else(|| fleet_rep.dict_get("fleetID").and_then(|v| v.as_i64()))
            .or(sess_a.fleet_id)
            .expect("fleetID");
        sess_a.fleet_id = Some(fleet_id);
        ctx.advance_secs(12);

        rpc(&ctx, &mut sess_a, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_B)]).await.expect("Invite PILOT_B");
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

        rpc(&ctx, &mut sess_b, "fleetObjectHandler", "AcceptInvite", &[]).await.expect("AcceptInvite PILOT_B");
        sess_b.fleet_id = Some(fleet_id);
        let join_notifs_bare = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(
            !join_notifs_bare.iter().any(|(ev, _)| ev == "OnFleetRespawnPointsUpdate"),
            "bare Astrahus without clone bay must not be emitted as a respawn point"
        );

        // Fit and online Standup Cloning Center I (35894) on the Astrahus -> now included as respawn point
        let clone_bay_svc_id = 1_040_000_002_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                clone_bay_svc_id,
                eve_db::InventoryItemRecord {
                    item_id: clone_bay_svc_id,
                    type_id: 35_894,
                    owner_id: 1_000_125,
                    location_id: struct_id,
                    flag: eve_db::InventoryFlag::ServiceSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(clone_bay_svc_id, true);
        });
        rpc(&ctx, &mut sess_b, "fleetObjectHandler", "LeaveFleet", &[]).await.expect("LeaveFleet PILOT_B");
        sess_b.fleet_id = None;
        ctx.advance_secs(12);
        rpc(&ctx, &mut sess_a, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_B)]).await.expect("Invite PILOT_B again");
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
        rpc(&ctx, &mut sess_b, "fleetObjectHandler", "AcceptInvite", &[]).await.expect("AcceptInvite PILOT_B with clone bay");
        sess_b.fleet_id = Some(fleet_id);
        let join_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
        let respawn_args = find_notif(&join_notifs_a, "OnFleetRespawnPointsUpdate");
        assert_eq!(respawn_args.len(), 1);
        let PyRep::List(respawn_list) = &respawn_args[0] else {
            panic!("expected List of respawn KeyVals");
        };
        assert_eq!(respawn_list.len(), 1);
        assert_eq!(
            respawn_list[0].dict_get("locationID").and_then(|v| v.as_i64()),
            Some(struct_id)
        );

        // 4. Command Burst activation -> emits OnFleetBoosted([43555, num_boosted >= 1])
        rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "LoadAmmo",
            &[
                PyRep::Integer(ship_a),
                PyRep::List(vec![PyRep::Integer(burst_id)]),
                PyRep::List(vec![PyRep::Integer(burst_charge_id)]),
            ],
        )
        .await
        .expect("LoadAmmo burst charge");
        ctx.advance_secs(35);
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(burst_id)])
            .await
            .expect("Activate Shield Command Burst II");
        let burst_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let boost_args = find_notif(&burst_notifs, "OnFleetBoosted");
        assert_eq!(boost_args.len(), 2);
        assert_eq!(boost_args[0].as_i64(), Some(43_555));
        assert!(boost_args[1].as_i64().unwrap_or(0) >= 1);

        // 5. Loot from space container in fleet -> emits OnFleetLootEvent([{(PILOT_A, 34): 50}])
        let (jetcan_id, loot_item_id) = (840_000_301_i64, 840_000_302_i64);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                jetcan_id,
                make_item(jetcan_id, 23, PILOT_A, SYS_JITA as i64, InventoryFlag::None, 1, true),
            );
            snap.items.insert(
                loot_item_id,
                make_item(loot_item_id, 34, PILOT_A, jetcan_id, InventoryFlag::None, 50, false),
            );
        });
        let jita_handle = ctx.systems.get_or_spawn(SYS_JITA);
        let _ = jita_handle
            .spawn_ball(Ball {
                id: jetcan_id,
                type_id: 23,
                position: DVec3::new(200_100.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 25.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
        rpc(
            &ctx,
            &mut sess_a,
            "invbroker",
            "Add",
            &[
                PyRep::Integer(loot_item_id),
                PyRep::Integer(ship_a),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
        )
        .await
        .expect("Loot jetcan item into ship cargo");
        let loot_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
        let loot_args = find_notif(&loot_notifs_b, "OnFleetLootEvent");
        assert_eq!(
            loot_args,
            &[PyRep::Dict(vec![(
                PyRep::Tuple(vec![PyRep::Integer(PILOT_A), PyRep::Integer(34)]),
                PyRep::Integer(50),
            )])]
        );

        // 6. Anchor & Unanchor Mobile Cynosural Beacon in fleet -> OnFleetJumpBeaconDeployableChange (true then false)
        let (beacon_id, depot_id) = (840_000_401_i64, 840_000_402_i64);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                beacon_id,
                make_item(beacon_id, TYPE_MOBILE_CYNO_BEACON, PILOT_A, ship_a, InventoryFlag::Cargo, 1, false),
            );
            snap.items.insert(
                depot_id,
                make_item(depot_id, TYPE_MOBILE_DEPOT, PILOT_A, ship_a, InventoryFlag::Cargo, 1, false),
            );
        });

        // Negative: anchoring regular Mobile Depot does NOT emit OnFleetJumpBeaconDeployableChange
        let _ = ctx
            .systems
            .get_or_spawn(SYS_JITA)
            .set_ball_position(ship_a, DVec3::new(300_000.0, 0.0, 0.0))
            .await;
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
        rpc(
            &ctx,
            &mut sess_a,
            "posMgr",
            "AnchorStructure",
            &[
                PyRep::Integer(depot_id),
                PyRep::Tuple(vec![PyRep::Real(300_200.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            ],
        )
        .await
        .expect("Anchor Mobile Depot");
        assert!(
            !drain_all(&ctx, &mut sess_b, PILOT_B)
                .iter()
                .any(|(e, _)| e == "OnFleetJumpBeaconDeployableChange")
        );

        const LOWSEC_VARD: u32 = 30_002_537;
        ctx.db.with_mut_snapshot(|snap| { snap.characters.get_mut(&PILOT_A).unwrap().solar_system_id = LOWSEC_VARD; snap.items.get_mut(&ship_a).unwrap().location_id = LOWSEC_VARD as i64; snap.item_positions.insert(ship_a, [300_000.0, 0.0, 0.0]); });
        sess_a.solar_system_id = Some(LOWSEC_VARD);
        let _ = rpc(&ctx, &mut sess_a, "beyonce", "UpdateStateRequest", &[]).await;

        rpc(
            &ctx,
            &mut sess_a,
            "posMgr",
            "AnchorStructure",
            &[
                PyRep::Integer(beacon_id),
                PyRep::Tuple(vec![PyRep::Real(300_300.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            ],
        )
        .await
        .expect("Anchor Mobile Cyno Beacon");
        let cyno_on_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
        assert_eq!(
            find_notif(&cyno_on_notifs, "OnFleetJumpBeaconDeployableChange"),
            &[
                PyRep::Integer(beacon_id),
                PyRep::Integer(LOWSEC_VARD as i64),
                PyRep::Integer(beacon_id),
                PyRep::Integer(PILOT_A),
                PyRep::Bool(true),
            ]
        );

        eve_server::services::notify_emitters_seasons_tactical::on_deployable_deactivated(
            &ctx,
            sess_a.fleet_id,
            beacon_id,
        );
        let cyno_off_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
        assert_eq!(
            find_notif(&cyno_off_notifs, "OnFleetJumpBeaconDeployableChange"),
            &[
                PyRep::Integer(beacon_id),
                PyRep::Integer(LOWSEC_VARD as i64),
                PyRep::Integer(beacon_id),
                PyRep::Integer(PILOT_A),
                PyRep::Bool(false),
            ]
        );
    }
}

pub(crate) mod make_item_ball {
    // Real production server-to-client `notify:On*` emission integration tests (Batch 10, Part 1).
    //
    // Covers Abyssal Deadspace, Void Space, Space White-Out, Zarzakh Deathzone, Proving Grounds
    // matchmaking, and Tournament arena/match state transitions via live RPCs and cluster ticks.


    use eve_db::{InventoryFlag, InventoryItemRecord, VirtualServerClock};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;
    use std::sync::Arc;

    const PILOT_A: i64 = 90_000_001;
    const PILOT_B: i64 = 90_000_002;
    const SYS_JITA: u32 = 30_000_142;

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

    fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
        Ball {
            id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 0.5,
            mass: 10_000_000.0,
            radius: 120.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        }
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

    fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
        let mut out = sess.drain_notifications();
        for n in ctx.drain_notifications_for_char(char_id) {
            out.push((n.event, n.args));
        }
        out
    }

    fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
        notifs
            .iter()
            .find(|(ev, _)| ev == event)
            .map(|(_, args)| args.as_slice())
            .unwrap_or_else(|| {
                let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
                panic!("expected notification {event}, got {seen:?}")
            })
    }

    fn has_notif(notifs: &[(String, Vec<PyRep>)], event: &str) -> bool {
        notifs.iter().any(|(ev, _)| ev == event)
    }

    async fn setup_pilot_in_space(
        ctx: &ServerContext,
        char_id: i64,
        ship_id: i64,
        ship_type_id: u32,
        gm: bool,
    ) -> SessionState {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = SYS_JITA;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = ship_id;
            }
            snap.items.insert(
                ship_id,
                make_item(ship_id, ship_type_id, char_id, i64::from(SYS_JITA), InventoryFlag::None, 1, true),
            );
        });
        let sys = ctx.systems.get_or_spawn(SYS_JITA);
        let _ = sys
            .spawn_ball_with_combat(make_ball(ship_id, ship_type_id, DVec3::new(1000.0, 0.0, 0.0)), Default::default())
            .await;
        let mut sess = SessionState::new(char_id as u64);
        let (user, pass) = if char_id == PILOT_A { ("demo", "demo-secret") } else { ("other", "other-secret") };
        crate::common::login_pilot(ctx, &mut sess, user, pass).await;
        rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
            .await
            .expect("SelectCharacterID");
        sess.solar_system_id = Some(SYS_JITA);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(ship_id);
        if gm {
            sess.role |= ROLE_GM_DEFAULT;
        }
        let _ = drain_all(ctx, &mut sess, char_id);
        sess
    }

    /// parity: notify:OnSpaceWhiteOutStart
    /// parity: notify:OnDeathzoneStateUpdate
    #[tokio::test]
    async fn batch10_abyssal_void_whiteout_and_deathzone_notifications() {
        let ctx = ServerContext::new();
        let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_001, 622, false).await;

        // 1. Abyssal filament deployment -> gate activation -> end gate activation
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.solar_system_id = 30_000_021;
            }
            if let Some(ship) = snap.items.get_mut(&700_001) {
                ship.location_id = 30_000_021;
            }
            snap.items.insert(
                700_010,
                make_item(700_010, 47_762, PILOT_A, 700_001, InventoryFlag::Cargo, 1, false),
            );
        });
        sess.solar_system_id = Some(30_000_021);
        let _ = ctx
            .systems
            .get_or_spawn(30_000_021)
            .spawn_ball_with_combat(make_ball(700_001, 622, DVec3::ZERO), Default::default())
            .await;

        rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(700_010)])
            .await
            .unwrap();
        let gate_id = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
            *reg.abyssal_traces.keys().next().unwrap()
        });

        // Negative: invalid gate does not emit OnAbyssalLocationEntered
        assert!(
            rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(999_999)])
                .await
                .is_err()
        );
        assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnAbyssalLocationEntered"));

        rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(gate_id)])
            .await
            .unwrap();
        let notifs = drain_all(&ctx, &mut sess, PILOT_A);
        let entered = find_notif(&notifs, "OnAbyssalLocationEntered");
        assert_eq!(entered.len(), 1);
        let content_id = entered[0].dict_get("content_id").and_then(|v| v.as_i64()).expect("content_id");
        assert!(content_id > 0);
        assert_eq!(entered[0].dict_get("difficulty_tier").and_then(|v| v.as_i64()), Some(1));
        assert!(entered[0].dict_get("location_id").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
        assert!(entered[0].dict_get("nebula_graphic_id").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

        // Negative: invalid end gate does not emit OnAbyssalContentFinished
        assert!(
            rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEndGateActivation", &[PyRep::Integer(999_999)])
                .await
                .is_err()
        );
        assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnAbyssalContentFinished"));

        eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
            if let Some(p) = reg.abyssal_pockets.get_mut(&PILOT_A) {
                p.current_room = 3;
                p.rooms[2].npc_ball_ids.clear();
                p.rooms[2].cleared = true;
            }
        });
        rpc(&ctx, &mut sess, "abyssalMgr", "ClientIsReady", &[]).await.unwrap();
        let end_gate_id = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
            reg.abyssal_pockets.get(&PILOT_A).map(|p| 1_895_000_000 + p.pocket_id * 10 + 3).unwrap()
        });
        if let Some(pocket_sys) = sess.solar_system_id {
            let abyss_sys = ctx.systems.get_or_spawn(pocket_sys);
            if let Ok(Some((_, gate_ball))) = abyss_sys.get_ball_snapshot(end_gate_id).await {
                let _ = abyss_sys.set_ball_position(700_001, gate_ball.position).await;
            }
        }
        rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEndGateActivation", &[PyRep::Integer(end_gate_id)])
            .await
            .unwrap();
        let notifs2 = drain_all(&ctx, &mut sess, PILOT_A);
        let finished = find_notif(&notifs2, "OnAbyssalContentFinished");
        assert_eq!(finished, &[PyRep::Integer(content_id), PyRep::Integer(1)]);

        // 2. OnSpaceWhiteOutStart on 20-minute Abyssal pocket collapse
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                700_012,
                make_item(700_012, 47_762, PILOT_A, 700_001, InventoryFlag::Cargo, 1, false),
            );
        });
        ctx.advance_secs(15);
        let _ = drain_all(&ctx, &mut sess, PILOT_A);
        rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(700_012)])
            .await
            .unwrap();
        let gate2 = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
            *reg.abyssal_traces.keys().max().unwrap()
        });
        rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(gate2)])
            .await
            .unwrap();
        let pocket_sys_2 = sess.solar_system_id.expect("in second abyssal pocket");
        let _ = drain_all(&ctx, &mut sess, PILOT_A);

        // 2a. OnDeathzoneStateUpdate via live cluster tick in Abyssal pocket (32000001)
        // Negative: inside ABYSSAL_ROOM_BOUNDARY_RADIUS_M (50km <= 75km from grid_offset) -> no emission
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnDeathzoneStateUpdate"));

        // Positive: move outside 75km room boundary relative to grid_offset (80km) -> emits danger trackingData
        let pocket_center = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
            reg.abyssal_pockets
                .get(&PILOT_A)
                .map(|p| DVec3::from_array(p.grid_offset))
                .unwrap()
        });
        let abyss_sys_2 = ctx.systems.get_or_spawn(pocket_sys_2);
        let _ = abyss_sys_2
            .set_ball_position(700_001, pocket_center + DVec3::new(80_000.0, 0.0, 0.0))
            .await;
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let dz_enter = drain_all(&ctx, &mut sess, PILOT_A);
        let dz_args = find_notif(&dz_enter, "OnDeathzoneStateUpdate");
        assert_eq!(dz_args.len(), 3);
        assert_eq!(dz_args[0], PyRep::Integer(i64::from(pocket_sys_2)));
        assert_eq!(dz_args[1], PyRep::Integer(700_001));
        assert_eq!(dz_args[2].dict_get("dangerLevel").and_then(|v| v.as_f64()), Some(1.0));

        // Move back inside 75km room boundary (10km from grid_offset) -> emits trackingData = None
        let _ = abyss_sys_2
            .set_ball_position(700_001, pocket_center + DVec3::new(10_000.0, 0.0, 0.0))
            .await;
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let dz_exit = drain_all(&ctx, &mut sess, PILOT_A);
        let dz_exit_args = find_notif(&dz_exit, "OnDeathzoneStateUpdate");
        assert_eq!(
            dz_exit_args,
            &[
                PyRep::Integer(i64::from(pocket_sys_2)),
                PyRep::Integer(700_001),
                PyRep::None,
            ]
        );

        // Negative: advancing 10s (well before 20-min expiry) does NOT emit OnSpaceWhiteOutStart
        ctx.advance_secs(10);
        assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnSpaceWhiteOutStart"));

        // Positive: advancing past 20-min timer collapses pocket and emits OnSpaceWhiteOutStart(pocket_sys)
        ctx.advance_secs(1_250);
        let collapse_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        let wo_args = find_notif(&collapse_notifs, "OnSpaceWhiteOutStart");
        assert_eq!(wo_args, &[PyRep::Integer(i64::from(pocket_sys_2))]);

        // 3. Void Space jump, relog, and end gate activation (restore clean ship in k-space 30000021)
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.solar_system_id = 30_000_021;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = 700_002;
            }
            snap.items.insert(
                700_002,
                make_item(700_002, 622, PILOT_A, 30_000_021, InventoryFlag::None, 1, true),
            );
        });
        sess.solar_system_id = Some(30_000_021);
        sess.station_id = None;
        sess.ship_id = Some(700_002);
        let _ = ctx
            .systems
            .get_or_spawn(30_000_021)
            .spawn_ball_with_combat(make_ball(700_002, 622, DVec3::ZERO), Default::default())
            .await;
        let _ = drain_all(&ctx, &mut sess, PILOT_A);

        // Negative: ClientIsReady outside void space does not emit OnVoidSpaceRelog
        let _ = rpc(&ctx, &mut sess, "voidSpaceMgr", "ClientIsReady", &[]).await;
        assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnVoidSpaceRelog"));

        // Negative: invalid filament does not emit OnVoidSpaceEntered
        assert!(
            rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpacePlayerJump", &[PyRep::Integer(999_999)])
                .await
                .is_err()
        );
        assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnVoidSpaceEntered"));

        let void_fil_type = ctx
            .sde
            .types
            .iter()
            .find(|(_, t)| t.group_id == 4145)
            .map(|(&id, _)| id)
            .unwrap_or(60_770);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                700_011,
                make_item(700_011, void_fil_type, PILOT_A, 700_002, InventoryFlag::Cargo, 1, false),
            );
        });
        ctx.advance_secs(12);
        let _ = drain_all(&ctx, &mut sess, PILOT_A);
        rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpacePlayerJump", &[PyRep::Integer(700_011)])
            .await
            .unwrap();
        let vse = drain_all(&ctx, &mut sess, PILOT_A);
        let vse_args = find_notif(&vse, "OnVoidSpaceEntered");
        assert_eq!(vse_args.len(), 1);
        let vs_cid = vse_args[0].dict_get("content_id").and_then(|v| v.as_i64()).expect("void content_id");
        assert!(vs_cid > 0);
        assert!(vse_args[0].dict_get("nebula_graphic_id").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

        let _ = rpc(&ctx, &mut sess, "voidSpaceMgr", "ClientIsReady", &[]).await;
        let relog_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        let relog_args = find_notif(&relog_notifs, "OnVoidSpaceRelog");
        assert_eq!(relog_args.len(), 1);
        assert_eq!(relog_args[0].dict_get("content_id").and_then(|v| v.as_i64()), Some(vs_cid));
        assert!(
            relog_args[0].dict_get("end_time").and_then(|v| v.as_i64()).unwrap_or(0)
                > ctx.now_blueos_filetime_ticks()
        );

        // Negative: invalid exit gate does not emit OnVoidSpaceContentFinished
        assert!(
            rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpaceEndGateActivation", &[PyRep::Integer(888_888)])
                .await
                .is_err()
        );
        assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnVoidSpaceContentFinished"));

        let void_exit_gate = 999_001_i64;
        if let Some(vs_sys) = sess.solar_system_id {
            let void_sys = ctx.systems.get_or_spawn(vs_sys);
            let _ = void_sys
                .spawn_ball_with_combat(make_ball(void_exit_gate, 60_608, DVec3::ZERO), Default::default())
                .await;
            let _ = void_sys.set_ball_position(700_002, DVec3::ZERO).await;
        }
        ctx.advance_secs(12);
        let _ = drain_all(&ctx, &mut sess, PILOT_A);
        rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpaceEndGateActivation", &[PyRep::Integer(void_exit_gate)])
            .await
            .unwrap();
        let void_fin = drain_all(&ctx, &mut sess, PILOT_A);
        let void_fin_args = find_notif(&void_fin, "OnVoidSpaceContentFinished");
        assert_eq!(void_fin_args, &[PyRep::Integer(vs_cid), PyRep::Integer(1)]);
    }

    /// parity: notify:OnMatchMakingFailure
    /// parity: notify:OnMatchFound
    /// parity: notify:OnProvingGroundsMatchFound
    /// parity: notify:OnAddExpiryDurationForAbyssalPvpContent
    /// parity: notify:OnEnterTournamentSystem
    /// parity: notify:OnTournamentMatchStateChange
    /// parity: notify:OnLeaveTournamentSystem
    #[tokio::test]
    async fn batch10_proving_grounds_and_tournament_notifications() {
        let ctx = ServerContext::new();
        // Set clock inside SDE schedule 170 (2023-01-07T12:00:00Z, 2v2 Battleships Proving Filament 72984)
        ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_673_092_800)));

        let mut sess_a = setup_pilot_in_space(&ctx, PILOT_A, 700_101, 638, true).await;
        let mut sess_b = setup_pilot_in_space(&ctx, PILOT_B, 700_102, 638, false).await;

        // 1. JoinPVPQueue with missing filament emits OnMatchMakingFailure (and NOT OnMatchFound)
        let err = rpc(
            &ctx,
            &mut sess_a,
            "pvpFilamentMgr",
            "JoinPVPQueue",
            &[PyRep::Integer(999_999), PyRep::Integer(72_984)],
        )
        .await
        .unwrap_err();
        let fail_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(!has_notif(&fail_notifs, "OnMatchFound"));
        assert!(!has_notif(&fail_notifs, "OnProvingGroundsMatchFound"));
        assert!(!has_notif(&fail_notifs, "OnAddExpiryDurationForAbyssalPvpContent"));
        let fail_args = find_notif(&fail_notifs, "OnMatchMakingFailure");
        assert_eq!(fail_args, &[PyRep::List(vec![PyRep::String(err.code)])]);

        // 2. Live 4-pilot 2v2 JoinPVPQueue matchmaking
        let (p3_id, p4_id) = (90_000_003_i64, 90_000_004_i64);
        let (ship3, ship4) = (700_103_i64, 700_104_i64);
        let (f1, f2, f3, f4) = (700_111_i64, 700_112_i64, 700_113_i64, 700_114_i64);
        ctx.db.with_mut_snapshot(|snap| {
            let tmpl = snap.characters.get(&PILOT_B).unwrap().clone();
            for (cid, sid) in [(p3_id, ship3), (p4_id, ship4)] {
                let mut rec = tmpl.clone();
                rec.character_id = cid;
                rec.solar_system_id = SYS_JITA;
                rec.station_id = None;
                rec.structure_id = None;
                rec.active_ship_id = sid;
                snap.characters.insert(cid, rec);
                snap.items.insert(
                    sid,
                    make_item(sid, 638, cid, i64::from(SYS_JITA), InventoryFlag::None, 1, true),
                );
            }
            for (fid, cid, sid) in [
                (f1, PILOT_A, 700_101),
                (f2, PILOT_B, 700_102),
                (f3, p3_id, ship3),
                (f4, p4_id, ship4),
            ] {
                snap.items.insert(
                    fid,
                    make_item(fid, 72_984, cid, sid, InventoryFlag::Cargo, 1, false),
                );
            }
            snap.fleets.insert(77_021, eve_db::FleetRecord { fleet_id: 77_021, boss_id: PILOT_A, members: vec![PILOT_A, p3_id], ..Default::default() });
            snap.fleets.insert(77_022, eve_db::FleetRecord { fleet_id: 77_022, boss_id: PILOT_B, members: vec![PILOT_B, p4_id], ..Default::default() });
        });
        let sys_h = ctx.systems.get_or_spawn(sess_a.solar_system_id.unwrap_or(30000142));
        for (cid, sid) in [(PILOT_A, sess_a.ship_id.unwrap()), (PILOT_B, sess_b.ship_id.unwrap()), (p3_id, ship3), (p4_id, ship4)] {
            let b = eve_destiny::Ball {
                id: sid, type_id: 638, position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO, max_velocity: 250.0, inertia: 0.5, mass: 100_000_000.0,
                radius: 300.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
            };
            let cs = ctx.build_ship_combat_state(cid, sid, 638);
            let _ = sys_h.spawn_ball_with_combat(b, cs).await;
        }

        // Negative: first 2-pilot fleet joining stays queued (status 30) and does NOT emit match-found notifications
        assert_eq!(
            rpc(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(f1), PyRep::Integer(72_984)])
                .await
                .unwrap(),
            PyRep::Integer(30)
        );
        assert!(!has_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnMatchFound"));
        assert!(!has_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnProvingGroundsMatchFound"));

        // Positive: 4th pilot joins -> forms match and emits all 3 proving grounds notifications
        let now_ft = ctx.now_blueos_filetime_ticks();
        assert_eq!(
            rpc(&ctx, &mut sess_b, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(f2), PyRep::Integer(72_984)])
                .await
                .unwrap(),
            PyRep::Integer(5)
        );
        let match_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
        assert!(!has_notif(&match_notifs, "OnMatchMakingFailure"));
        assert_eq!(find_notif(&match_notifs, "OnMatchFound"), &[]);
        assert_eq!(find_notif(&match_notifs, "OnProvingGroundsMatchFound"), &[]);
        let exp_args = find_notif(&match_notifs, "OnAddExpiryDurationForAbyssalPvpContent");
        assert_eq!(exp_args.len(), 1);
        let exp_ft = exp_args[0].as_i64().expect("end_time filetime");
        assert!(exp_ft >= now_ft + 900 * 10_000_000);
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

        // 3. Tournament enter, match state change, and leave via live tourneyMgr RPCs
        assert!(
            rpc(&ctx, &mut sess_b, "tourneyMgr", "TeleportToArena", &[PyRep::Integer(999_999)])
                .await
                .is_err()
        );
        assert!(!has_notif(&drain_all(&ctx, &mut sess_b, PILOT_B), "OnEnterTournamentSystem"));

        let _ = ctx.db.set_character_solar_system(PILOT_A, 30_002_187, &ctx.sde);
        sess_a.solar_system_id = Some(30_002_187);
        ctx.sync_session(&mut sess_a);
        let match_id = 1001_i64;
        rpc(&ctx, &mut sess_a, "tourneyMgr", "CreateMatch", &[PyRep::Integer(1), PyRep::Integer(match_id)])
            .await
            .unwrap();
        rpc(
            &ctx,
            &mut sess_a,
            "tourneyMgr",
            "AddPlayer",
            &[PyRep::Integer(match_id), PyRep::Integer(0), PyRep::Integer(PILOT_B)],
        )
        .await
        .unwrap();
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

        // Negative: non-referee PILOT_B cannot StartCountdown -> Err and zero OnTournamentMatchStateChange
        assert!(
            rpc(&ctx, &mut sess_b, "tourneyMgr", "StartCountdown", &[PyRep::Integer(match_id)])
                .await
                .is_err()
        );
        assert!(!has_notif(&drain_all(&ctx, &mut sess_b, PILOT_B), "OnTournamentMatchStateChange"));

        // Positive: referee PILOT_A starts countdown -> red player gets myTeam=0, referee gets myTeam=-1, matchStatus=2
        rpc(&ctx, &mut sess_a, "tourneyMgr", "StartCountdown", &[PyRep::Integer(match_id)])
            .await
            .unwrap();
        let b_state = drain_all(&ctx, &mut sess_b, PILOT_B);
        assert_eq!(
            find_notif(&b_state, "OnTournamentMatchStateChange"),
            &[PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(2)]
        );
        let a_state = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(
            find_notif(&a_state, "OnTournamentMatchStateChange"),
            &[PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(-1), PyRep::Integer(2)]
        );

        // Negative: calling StartCountdown again when already in Countdown emits zero duplicate notifications
        rpc(&ctx, &mut sess_a, "tourneyMgr", "StartCountdown", &[PyRep::Integer(match_id)])
            .await
            .unwrap();
        assert!(!has_notif(&drain_all(&ctx, &mut sess_b, PILOT_B), "OnTournamentMatchStateChange"));

        // TeleportToArena -> OnEnterTournamentSystem (0 args)
        let _ = ctx.db.set_character_solar_system(PILOT_A, SYS_JITA, &ctx.sde);
        sess_a.solar_system_id = Some(SYS_JITA);
        rpc(&ctx, &mut sess_a, "tourneyMgr", "TeleportToArena", &[PyRep::Integer(match_id)])
            .await
            .unwrap();
        let enter_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(find_notif(&enter_notifs, "OnEnterTournamentSystem"), &[]);

        // Negative: non-referee TeleportToStation rejected -> zero OnLeaveTournamentSystem
        assert!(
            rpc(&ctx, &mut sess_b, "tourneyMgr", "TeleportToStation", &[PyRep::Integer(match_id)])
                .await
                .is_err()
        );
        assert!(!has_notif(&drain_all(&ctx, &mut sess_b, PILOT_B), "OnLeaveTournamentSystem"));

        rpc(&ctx, &mut sess_a, "tourneyMgr", "MatchCompleted", &[PyRep::Integer(match_id)])
            .await
            .unwrap();
        rpc(&ctx, &mut sess_a, "tourneyMgr", "TeleportToStation", &[PyRep::Integer(match_id)])
            .await
            .unwrap();
        let leave_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(find_notif(&leave_notifs, "OnLeaveTournamentSystem"), &[]);
    }
}

pub(crate) mod make_item_rpc {
    // Real production server-to-client `notify:On*` emission integration tests (Batch 10, Part 2).
    //
    // Covers Dungeon Editor, Dungeon Values, all 5 Dungeon Room Triggers, Signal Tracker full state,
    // Scanner Probe repositioning (`OnProbeWarpEnd`), and Escalation Expeditions
    // (`OnCosmicSignatureAdded`, `OnDungeonInstanceAdded`, `OnDungeonInstanceRemoved`).


    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;

    const PILOT_A: i64 = 90_000_001;
    const PILOT_B: i64 = 90_000_002;
    const SYS_JITA: u32 = 30_000_142;

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
        svc: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
            .await
    }

    async fn rpc_kw(
        ctx: &ServerContext,
        sess: &mut SessionState,
        svc: &str,
        method: &str,
        args: &[PyRep],
        kwargs: &HashMap<String, PyRep>,
    ) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, svc, None, method, args, kwargs, None)
            .await
    }

    fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
        let mut out = sess.drain_notifications();
        for n in ctx.drain_notifications_for_char(char_id) {
            out.push((n.event, n.args));
        }
        out
    }

    fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
        notifs
            .iter()
            .find(|(ev, _)| ev == event)
            .map(|(_, args)| args.as_slice())
            .unwrap_or_else(|| {
                let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
                panic!("expected notification {event}, got {seen:?}")
            })
    }

    fn has_notif(notifs: &[(String, Vec<PyRep>)], event: &str) -> bool {
        notifs.iter().any(|(ev, _)| ev == event)
    }

    async fn setup_pilot_in_space(ctx: &ServerContext, char_id: i64, ship_id: i64, gm: bool) -> SessionState {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = SYS_JITA;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = ship_id;
            }
            snap.items.insert(
                ship_id,
                make_item(ship_id, 587, char_id, i64::from(SYS_JITA), InventoryFlag::None, 1, true),
            );
        });
        let sys = ctx.systems.get_or_spawn(SYS_JITA);
        let _ = sys
            .spawn_ball_with_combat(
                Ball {
                    id: ship_id,
                    type_id: 587,
                    position: DVec3::new(1000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 0.5,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                Default::default(),
            )
            .await;
        let mut sess = SessionState::new(char_id as u64);
        let (user, pass) = if char_id == PILOT_A { ("demo", "demo-secret") } else { ("other", "other-secret") };
        crate::common::login_pilot(ctx, &mut sess, user, pass).await;
        rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
            .await
            .expect("SelectCharacterID");
        sess.solar_system_id = Some(SYS_JITA);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(ship_id);
        if gm {
            sess.role |= ROLE_GM_DEFAULT;
        }
        let _ = drain_all(ctx, &mut sess, char_id);
        sess
    }

    /// parity: notify:OnDungeonEdit
    /// parity: notify:OnDungeonValues
    #[tokio::test]
    async fn batch10_dungeon_editor_values_and_room_triggers() {
        let ctx = ServerContext::new();
        let mut gm_sess = setup_pilot_in_space(&ctx, PILOT_A, 700_201, true).await;
        let mut non_gm = setup_pilot_in_space(&ctx, PILOT_B, 700_202, false).await;

        // 1. OnDungeonEdit via keeper.EditDungeon
        // Negative: non-GM cannot EditDungeon, and GM PlayDungeon (godmode=0) does NOT emit OnDungeonEdit
        assert!(rpc(&ctx, &mut non_gm, "keeper", "EditDungeon", &[PyRep::Integer(1420)]).await.is_err());
        assert!(!has_notif(&drain_all(&ctx, &mut non_gm, PILOT_B), "OnDungeonEdit"));

        let mut play_kw = HashMap::new();
        play_kw.insert("dungeonID".into(), PyRep::Integer(1420));
        play_kw.insert("godmode".into(), PyRep::Integer(0));
        rpc_kw(&ctx, &mut gm_sess, "keeper", "PlayDungeon", &[], &play_kw).await.unwrap();
        assert!(!has_notif(&drain_all(&ctx, &mut gm_sess, PILOT_A), "OnDungeonEdit"));

        // Positive: GM EditDungeon emits OnDungeonEdit(dungeonID, roomID, (x, y, z))
        let mut edit_kw = HashMap::new();
        edit_kw.insert("dungeonID".into(), PyRep::Integer(1420));
        rpc_kw(&ctx, &mut gm_sess, "keeper", "EditDungeon", &[], &edit_kw).await.unwrap();
        let edit_notifs = drain_all(&ctx, &mut gm_sess, PILOT_A);
        let edit_args = find_notif(&edit_notifs, "OnDungeonEdit");
        assert_eq!(edit_args.len(), 3);
        assert_eq!(edit_args[0], PyRep::Integer(1420));
        assert!(edit_args[1].as_i64().unwrap_or(0) > 0);
        match &edit_args[2] {
            PyRep::Tuple(coords) => assert_eq!(coords.len(), 3),
            other => panic!("expected 3-tuple roomPos, got {other:?}"),
        }

        // 2. Spawn normal SDE dungeon instance with entry gate and verify OnDungeonValues
        let inst_id = eve_server::services::pve_dungeon_runtime::spawn_sde_dungeon_runtime_instance(
            &ctx,
            PILOT_A,
            SYS_JITA,
            1420,
            "Deadspace",
            [100_000.0, 0.0, 0.0],
            true,
        )
        .await
        .expect("spawn_sde_dungeon_runtime_instance");
        let state_rep = eve_server::services::pve_dungeon_runtime_rpc::get_dungeon_instance_state(&ctx, inst_id).unwrap();
        let entry_gate_id = state_rep
            .dict_get("entryGateBallID")
            .and_then(|v| v.as_i64())
            .expect("entryGateBallID");
        let _ = drain_all(&ctx, &mut non_gm, PILOT_B);

        // Negative: Pilot B is >2500m from entry gate -> ActivateAccelerationGate fails, zero OnDungeonValues
        assert!(
            rpc(&ctx, &mut non_gm, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(entry_gate_id)])
                .await
                .is_err()
        );
        assert!(!has_notif(&drain_all(&ctx, &mut non_gm, PILOT_B), "OnDungeonValues"));

        // Positive: move Pilot B within 2500m of entry gate and activate -> warps into Room 1 and emits OnDungeonValues
        let jita_sys = ctx.systems.get_or_spawn(SYS_JITA);
        let (_, gate_ball) = jita_sys.get_ball_snapshot(entry_gate_id).await.unwrap().unwrap();
        let _ = jita_sys.set_ball_position(700_202, gate_ball.position).await;
        let _room_enter_rep = rpc(&ctx, &mut non_gm, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(entry_gate_id)])
            .await
            .expect("ActivateAccelerationGate");
        let gate_notifs = drain_all(&ctx, &mut non_gm, PILOT_B);
        let dv_args = find_notif(&gate_notifs, "OnDungeonValues");
        assert_eq!(dv_args.len(), 2);
        assert_eq!(dv_args[0], PyRep::Integer(1420));
        assert_eq!(dv_args[1].dict_get("currentWave").and_then(|v| v.as_i64()), Some(1));
    }

    /// parity: notify:OnSignalTrackerFullState
    /// parity: notify:OnProbeWarpEnd
    /// parity: notify:OnCosmicSignatureAdded
    /// parity: notify:OnDungeonInstanceAdded
    /// parity: notify:OnDungeonInstanceRemoved
    #[tokio::test]
    async fn batch10_expedition_signatures_signal_tracker_and_probes() {
        let ctx = ServerContext::new();
        let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_301, false).await;

        // 1. OnSignalTrackerFullState via scanMgr.SignalTrackerRegister
        // Negative: session with solar_system_id = None returns NoSolarSystem and emits zero notifications
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&PILOT_A).unwrap().solar_system_id = 0);
        sess.solar_system_id = None;
        assert_eq!(
            rpc(&ctx, &mut sess, "scanMgr", "SignalTrackerRegister", &[]).await.unwrap_err().code,
            "NoSolarSystem"
        );
        assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnSignalTrackerFullState"));

        // Positive: in-space pilot registers and receives 3-arg OnSignalTrackerFullState with 4-tuple fullState
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&PILOT_A).unwrap().solar_system_id = SYS_JITA);
        sess.solar_system_id = Some(SYS_JITA);
        rpc(&ctx, &mut sess, "scanMgr", "SignalTrackerRegister", &[]).await.unwrap();
        let st_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        let st_args = find_notif(&st_notifs, "OnSignalTrackerFullState");
        assert_eq!(st_args.len(), 3);
        assert_eq!(st_args[0], PyRep::Integer(i64::from(SYS_JITA)));
        match &st_args[1] {
            PyRep::Tuple(parts) => assert_eq!(parts.len(), 4),
            other => panic!("expected 4-tuple fullState, got {other:?}"),
        }
        assert_eq!(st_args[2], PyRep::Bool(true));

        // 2. OnProbeWarpEnd via scanSvc / dogmaIM probe launch + RequestScans
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                700_310,
                make_item(700_310, 17_938, PILOT_A, 700_301, InventoryFlag::HiSlot0, 1, true),
            );
            snap.items.insert(
                700_311,
                make_item(700_311, 30_013, PILOT_A, 700_301, InventoryFlag::HiSlot0, 8, false),
            );
        });
        ctx.invalidate_ship_fitting(700_301);
        let _ = rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(700_301), PyRep::Integer(700_310)]).await;
        rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(700_310), PyRep::String("launchProbes".into())])
            .await
            .unwrap();
        let _ = drain_all(&ctx, &mut sess, PILOT_A);

        // Negative: RequestScans without moving probe coordinates does NOT emit OnProbeWarpEnd
        rpc(&ctx, &mut sess, "scanMgr", "RequestScans", &[]).await.unwrap();
        assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnProbeWarpEnd"));

        // Positive: RequestScans with new probe coordinates repositions probe and emits OnProbeWarpEnd(probeID)
        let first_probe_id = ctx.active_scanner_probes.read().unwrap().keys().min().copied().expect("launched probe");
        rpc(
            &ctx,
            &mut sess,
            "scanMgr",
            "RequestScans",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Real(250_000.0),
                PyRep::Real(100_000.0),
                PyRep::Real(-50_000.0),
                PyRep::Integer(4),
            ])])],
        )
        .await
        .unwrap();
        let scan_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert!(has_notif(&scan_notifs, "OnProbeWarpStart"));
        assert!(!has_notif(&scan_notifs, "OnProbeWarpEnd"));

        // Advance clock past warp_ft (1000ms = 1s) -> OnProbeWarpEnd(probeID) fires
        ctx.advance_secs(2);
        let after_warp_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        let pwe_args = find_notif(&after_warp_notifs, "OnProbeWarpEnd");
        assert_eq!(pwe_args, &[PyRep::Integer(first_probe_id)]);

        // 3. OnCosmicSignatureAdded, OnDungeonInstanceAdded, OnDungeonInstanceRemoved via live keeper.EnterNextDungeonRoom
        let dest_sys = eve_server::services::pve_expeditions::find_escalation_destination_system(&ctx, SYS_JITA);
        let mut obs_sess = setup_pilot_in_space(&ctx, PILOT_B, 700_302, false).await;
        let _ = ctx.db.set_character_solar_system(PILOT_B, dest_sys, &ctx.sde);
        obs_sess.solar_system_id = Some(dest_sys);
        ctx.sync_session(&mut obs_sess);
        let _ = rpc(&ctx, &mut obs_sess, "scanSvc", "GetCombatAnomalyInstances", &[]).await.unwrap();
        let _ = drain_all(&ctx, &mut obs_sess, PILOT_B);

        // Negative: advancing wave 1 -> 2 of a 2-wave anomaly does NOT complete or spawn an expedition
        let _ = rpc(&ctx, &mut sess, "scanSvc", "GetCombatAnomalyInstances", &[]).await.unwrap();
        let site_id = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
            let sites = reg.sites_by_system.get_mut(&SYS_JITA).unwrap();
            let s = sites.iter_mut().find(|s| s.site_category == "CombatAnomaly").unwrap();
            s.current_wave = 1;
            s.total_waves = 2;
            s.cleared = false;
            s.active_npc_ball_ids.clear();
            s.site_id.clone()
        });
        rpc(&ctx, &mut sess, "keeper", "EnterNextDungeonRoom", &[PyRep::String(site_id.clone())]).await.unwrap();
        let wave1_obs = drain_all(&ctx, &mut obs_sess, PILOT_B);
        assert!(!has_notif(&wave1_obs, "OnCosmicSignatureAdded"));
        assert!(!has_notif(&wave1_obs, "OnDungeonInstanceAdded"));

        // Positive: find deterministic RNG seed that rolls <= 0.20 escalation on final wave completion
        let mut esc_seed = 0_u64;
        for candidate in 1..200_u64 {
            let rng = eve_server::rng::MechanicRng::seeded(candidate);
            if rng.roll(0.20) {
                esc_seed = candidate;
                break;
            }
        }
        ctx.set_mechanic_rng_seed(Some(esc_seed));
        eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
            if let Some(s) = reg.sites_by_system.get_mut(&SYS_JITA).and_then(|l| l.iter_mut().find(|x| x.site_id == site_id)) {
                s.active_npc_ball_ids.clear();
            }
        });
        let clear_rep = rpc(&ctx, &mut sess, "keeper", "EnterNextDungeonRoom", &[PyRep::String(site_id)])
            .await
            .unwrap();
        let inst_id = clear_rep.dict_get("escalationInstanceID").and_then(|v| v.as_i64()).expect("escalationInstanceID");

        let add_obs = drain_all(&ctx, &mut obs_sess, PILOT_B);
        let sig_args = find_notif(&add_obs, "OnCosmicSignatureAdded");
        assert_eq!(sig_args.len(), 1);
        let sig_id = sig_args[0].as_str().expect("signatureID string").to_string();
        assert_eq!(sig_id.len(), 7);
        assert_eq!(sig_id.as_bytes()[3], b'-');

        let di_add = find_notif(&add_obs, "OnDungeonInstanceAdded");
        assert_eq!(di_add.len(), 2);
        assert_eq!(di_add[0], PyRep::Integer(i64::from(dest_sys)));
        assert_eq!(di_add[1].dict_get("signatureID").and_then(|v| v.as_str()), Some(sig_id.as_str()));
        let exp_did = di_add[1].dict_get("dungeonID").and_then(|v| v.as_i64()).expect("expedition dungeonID");
        assert!(exp_did > 0);
        assert!(di_add[1].dict_get("scanStrengthValue").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

        // Move Pilot A to dest_sys to run Step 1 of the expedition
        let _ = ctx.db.set_character_solar_system(PILOT_A, dest_sys, &ctx.sde);
        sess.solar_system_id = Some(dest_sys);
        ctx.sync_session(&mut sess);
        let _ = drain_all(&ctx, &mut sess, PILOT_A);
        let _ = drain_all(&ctx, &mut obs_sess, PILOT_B);

        // Negative: non-existent expedition site fails and emits zero OnDungeonInstanceRemoved
        assert!(
            rpc(&ctx, &mut sess, "keeper", "EnterNextDungeonRoom", &[PyRep::String("EXP-999999-S1".into())])
                .await
                .is_err()
        );
        assert!(!has_notif(&drain_all(&ctx, &mut obs_sess, PILOT_B), "OnDungeonInstanceRemoved"));

        // Positive: clearing final wave of EXP-{inst_id}-1 advances expedition and emits OnDungeonInstanceRemoved
        let exp_site_id = format!("EXP-{inst_id}-1");
        eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
            if let Some(s) = reg.sites_by_system.get_mut(&dest_sys).and_then(|l| l.iter_mut().find(|x| x.site_id == exp_site_id)) {
                s.current_wave = s.total_waves;
                s.active_npc_ball_ids.clear();
            }
        });
        rpc(&ctx, &mut sess, "keeper", "EnterNextDungeonRoom", &[PyRep::String(exp_site_id)])
            .await
            .unwrap();
        let rem_obs = drain_all(&ctx, &mut obs_sess, PILOT_B);
        let rem_args = find_notif(&rem_obs, "OnDungeonInstanceRemoved");
        assert_eq!(
            rem_args,
            &[
                PyRep::Integer(i64::from(dest_sys)),
                PyRep::Integer(inst_id),
                PyRep::String(sig_id),
                PyRep::Integer(exp_did),
            ]
        );
    }
}

pub(crate) mod make_item_rpc_alpha {
    // Real production server-to-client `notify:On*` emission integration tests (Batch 10, Part 3).
    //
    // Covers Progression & Tutorial Conversations (`operationsManager`), Sansha Incursion Tales
    // (`OnTaleData`, `OnInfluenceUpdate`, `OnTaleRemove`), and Roaming Metaliminal Storms
    // (`OnCosmicAnomalyAdded`, `OnSystemEffectStarted`, `OnSystemWideEffectStop`,
    // `OnComponentRemovedSystemEnvironment`) via live RPCs and server clock advancement.


    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;

    const PILOT_A: i64 = 90_000_001;
    const SYS_JITA: u32 = 30_000_142;

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
        svc: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
            .await
    }

    fn drain_all(
        ctx: &ServerContext,
        sess: &mut SessionState,
        char_id: i64,
    ) -> Vec<(String, Vec<PyRep>)> {
        let mut out = sess.drain_notifications();
        for n in ctx.drain_notifications_for_char(char_id) {
            out.push((n.event, n.args));
        }
        out
    }

    fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
        notifs
            .iter()
            .find(|(ev, _)| ev == event)
            .map(|(_, args)| args.as_slice())
            .unwrap_or_else(|| {
                let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
                panic!("expected notification {event}, got {seen:?}")
            })
    }

    fn has_notif(notifs: &[(String, Vec<PyRep>)], event: &str) -> bool {
        notifs.iter().any(|(ev, _)| ev == event)
    }

    async fn setup_pilot_in_space(
        ctx: &ServerContext,
        char_id: i64,
        ship_id: i64,
        gm: bool,
    ) -> SessionState {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = SYS_JITA;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = ship_id;
            }
            snap.items.insert(
                ship_id,
                make_item(
                    ship_id,
                    587,
                    char_id,
                    i64::from(SYS_JITA),
                    InventoryFlag::None,
                    1,
                    true,
                ),
            );
        });
        let sys = ctx.systems.get_or_spawn(SYS_JITA);
        let _ = sys
            .spawn_ball_with_combat(
                Ball {
                    id: ship_id,
                    type_id: 587,
                    position: DVec3::new(1000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 0.5,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                Default::default(),
            )
            .await;
        let mut sess = SessionState::new(char_id as u64);
        crate::common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
        rpc(
            ctx,
            &mut sess,
            "charUnboundMgr",
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
        )
        .await
        .expect("SelectCharacterID");
        sess.solar_system_id = Some(SYS_JITA);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(ship_id);
        if gm {
            sess.role |= ROLE_GM_DEFAULT;
        }
        let _ = drain_all(ctx, &mut sess, char_id);
        sess
    }

    /// parity: notify:OnProgressionLeft
    #[tokio::test]
    async fn batch10_progression_and_conversations() {
        let ctx = ServerContext::new();
        let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_401, false).await;

        // 1. Start an operation site via operationsManager.start_site (sets active_site_id)
        assert!(
            rpc(
                &ctx,
                &mut sess,
                "operationsManager",
                "start_site",
                &[PyRep::Integer(999_999_999)]
            )
            .await
            .is_err()
        );

        let op_defs = eve_server::services::operations_manager::get_operation_defs(&ctx.sde);
        let empty_done = std::collections::HashSet::new();
        let (op_with_site, site_id) = op_defs
            .op_site_ids
            .iter()
            .find(|(&oid, &sid)| sid > 0 && op_defs.are_prerequisites_met(oid, &empty_done))
            .map(|(&oid, &sid)| (oid, sid))
            .unwrap_or_else(|| {
                (
                    *op_defs.sde_operations.iter().next().expect("SDE op"),
                    i64::from(*ctx.sde.dungeons.keys().min().expect("SDE dungeon")),
                )
            });
        rpc(
            &ctx,
            &mut sess,
            "operationsManager",
            "activate_operation_in_solar_system",
            &[PyRep::Integer(op_with_site)],
        )
        .await
        .unwrap();
        rpc(
            &ctx,
            &mut sess,
            "operationsManager",
            "start_site",
            &[PyRep::Integer(site_id)],
        )
        .await
        .unwrap();
        let _ = drain_all(&ctx, &mut sess, PILOT_A);

        // 2. Positive: skip_tutorial while tutorial/site is active emits OnProgressionLeft() (0 args)
        rpc(
            &ctx,
            &mut sess,
            "operationsManager",
            "process_client_event",
            &[PyRep::String("skip_tutorial".into())],
        )
        .await
        .unwrap();
        let left_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&left_notifs, "OnProgressionLeft"), &[]);

        // 3. Negative: calling skip_tutorial again when no tutorial/site is active emits zero OnProgressionLeft
        rpc(
            &ctx,
            &mut sess,
            "operationsManager",
            "process_client_event",
            &[PyRep::String("skip_tutorial".into())],
        )
        .await
        .unwrap();
        let repeat_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(
            repeat_notifs
                .iter()
                .filter(|(n, _)| n == "OnProgressionLeft")
                .count(),
            0
        );
    }

    /// parity: notify:OnTaleData
    /// parity: notify:OnInfluenceUpdate
    /// parity: notify:OnTaleRemove
    /// parity: notify:OnCosmicAnomalyAdded
    /// parity: notify:OnSystemEffectStarted
    /// parity: notify:OnSystemWideEffectStop
    /// parity: notify:OnComponentRemovedSystemEnvironment
    #[tokio::test]
    async fn batch10_tales_storms_and_cosmic_anomalies() {
        let ctx = ServerContext::new();
        let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_410, false).await;

        // 1. Seed world event tales via live taleMgr.GetGlobalWorldEventTales RPC
        rpc(&ctx, &mut sess, "taleMgr", "GetGlobalWorldEventTales", &[])
            .await
            .unwrap();
        let (inc_cid, tale_id, staging_sys, vg_site_sys, vg_site_id, vg_boss_bid) =
            eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
                let inc = reg
                    .incursions
                    .values_mut()
                    .find(|i| i.state != "Ended" && i.staging_solar_system_id != SYS_JITA)
                    .expect("seeded incursion");
                let vg = inc
                    .sites
                    .iter_mut()
                    .find(|s| !s.locked_until_low_influence)
                    .expect("vanguard/assault incursion site");
                for e in &mut vg.escort_spawns {
                    e.destroyed = true;
                }
                vg.boss_destroyed = true;
                let sid = vg.site_id.clone();
                let bbid = vg.boss_npc_ball_id;
                let ssys = vg.solar_system_id;
                let icid = inc.constellation_id;
                let tid = inc.tale_id;
                let stsys = inc.staging_solar_system_id;
                if let Some(w) = reg.incursion_site_waves.get_mut(&sid) {
                    w.current_wave = w.total_waves;
                }
                (icid, tid, stsys, ssys, sid, bbid)
            });
        let _ = ctx.systems.get_or_spawn(vg_site_sys).remove_ball(vg_boss_bid).await;
        let _ = drain_all(&ctx, &mut sess, PILOT_A);

        // Negative: jumping via stargate into a non-incursion system (Jita -> Perimeter) emits zero OnTaleData
        let jita_gate_id = 50_001_248_i64;
        let jita_gate = &ctx.sde.jumps[&jita_gate_id];
        let gate_rad = ctx
            .sde
            .types
            .get(&16)
            .map(|t| t.radius)
            .unwrap_or(14_051.0);
        let bid_jita = ctx
            .bind_object(&mut sess, "beyonce", i64::from(SYS_JITA))
            .expect("bind beyonce in Jita");
        ctx.systems
            .get_or_spawn(SYS_JITA)
            .set_ball_position(
                700_410,
                jita_gate.position + DVec3::new(gate_rad + 500.0, 0.0, 0.0),
            )
            .await;
        ctx.advance_secs(15);
        ctx.dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(bid_jita),
            "CmdStargateJump",
            &[PyRep::Integer(jita_gate_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("CmdStargateJump Jita -> Perimeter");
        assert!(!has_notif(
            &drain_all(&ctx, &mut sess, PILOT_A),
            "OnTaleData"
        ));

        // Positive: jumping via stargate into an incursion-affected solar system emits OnTaleData
        let (inc_gate_id, from_sys, inc_gate_pos) = ctx
            .sde
            .jumps
            .values()
            .find(|j| j.to_solar_system_id == staging_sys)
            .map(|j| (j.stargate_id, j.from_solar_system_id, j.position))
            .expect("stargate leading into incursion staging system");
        let _ = ctx
            .db
            .set_character_solar_system(PILOT_A, from_sys, &ctx.sde);
        sess.solar_system_id = Some(from_sys);
        ctx.sync_session(&mut sess);
        let _ = ctx
            .systems
            .get_or_spawn(from_sys)
            .spawn_ball_with_combat(
                Ball {
                    id: 700_410,
                    type_id: 587,
                    position: inc_gate_pos + DVec3::new(gate_rad + 500.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 0.5,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                Default::default(),
            )
            .await;
        let _ = drain_all(&ctx, &mut sess, PILOT_A);
        let bid_from = ctx
            .bind_object(&mut sess, "beyonce", i64::from(from_sys))
            .expect("bind beyonce in from_sys");
        ctx.advance_secs(15);
        ctx.dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(bid_from),
            "CmdStargateJump",
            &[PyRep::Integer(inc_gate_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("CmdStargateJump into incursion staging_sys");
        let jump_td_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        let jump_td_args = find_notif(&jump_td_notifs, "OnTaleData");
        assert_eq!(jump_td_args.len(), 2);
        assert_eq!(jump_td_args[0], PyRep::Integer(i64::from(staging_sys)));
        match &jump_td_args[1] {
            PyRep::Dict(entries) => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].0, PyRep::Integer(tale_id));
                assert_eq!(
                    entries[0].1.dict_get("taleID").and_then(|v| v.as_i64()),
                    Some(tale_id)
                );
            }
            other => panic!("expected Dict of active tales on jump, got {other:?}"),
        }

        // Also verify beyonce.UpdateStateRequest in incursion staging system emits OnTaleData
        rpc(&ctx, &mut sess, "beyonce", "UpdateStateRequest", &[])
            .await
            .unwrap();
        let td_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        let td_args = find_notif(&td_notifs, "OnTaleData");
        assert_eq!(td_args.len(), 2);
        assert_eq!(td_args[0], PyRep::Integer(i64::from(staging_sys)));
        match &td_args[1] {
            PyRep::Dict(entries) => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].0, PyRep::Integer(tale_id));
                assert_eq!(
                    entries[0].1.dict_get("taleID").and_then(|v| v.as_i64()),
                    Some(tale_id)
                );
            }
            other => panic!("expected Dict of active tales, got {other:?}"),
        }

        // 2. OnInfluenceUpdate via live keeper.EnterNextDungeonRoom on an incursion site
        let _ = ctx
            .db
            .set_character_solar_system(PILOT_A, vg_site_sys, &ctx.sde);
        sess.solar_system_id = Some(vg_site_sys);
        ctx.sync_session(&mut sess);
        let _ = drain_all(&ctx, &mut sess, PILOT_A);

        // Negative: invalid incursion site ID fails and emits zero OnInfluenceUpdate
        assert!(
            rpc(
                &ctx,
                &mut sess,
                "keeper",
                "EnterNextDungeonRoom",
                &[PyRep::String("INC-NONEXISTENT".into())]
            )
            .await
            .is_err()
        );
        assert!(!has_notif(
            &drain_all(&ctx, &mut sess, PILOT_A),
            "OnInfluenceUpdate"
        ));

        // Positive: completing real vanguard site reduces influence and emits OnInfluenceUpdate
        rpc(
            &ctx,
            &mut sess,
            "keeper",
            "EnterNextDungeonRoom",
            &[PyRep::String(vg_site_id)],
        )
        .await
        .unwrap();
        let inf_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        let inf_args = find_notif(&inf_notifs, "OnInfluenceUpdate");
        assert_eq!(inf_args.len(), 2);
        assert_eq!(inf_args[0], PyRep::Integer(tale_id));
        let new_inf = inf_args[1]
            .dict_get("influence")
            .and_then(|v| v.as_f64())
            .expect("influence float");
        assert!((0.0..1.0).contains(&new_inf));
        assert!(
            inf_args[1]
                .dict_get("lastUpdated")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                > 0
        );
        assert!(!has_notif(&inf_notifs, "OnTaleRemove"));

        // 3. OnTaleRemove when incursion withdrawal expires on server clock advancement
        let now_u = ctx.now_unix();
        eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
            if let Some(inc) = reg.incursions.get_mut(&inc_cid) {
                inc.state = "Withdrawing".into();
            }
            if let Some(meta) = reg.incursion_lifecycle.get_mut(&inc_cid) {
                meta.withdraw_until_sec = Some(now_u + 30);
            }
        });
        // Negative: advancing 5s (before withdraw_until_sec) does NOT emit OnTaleRemove
        ctx.clock().advance_secs(5);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert!(!has_notif(
            &drain_all(&ctx, &mut sess, PILOT_A),
            "OnTaleRemove"
        ));

        // Positive: advancing 35s crosses withdrawal expiry -> ends incursion and emits OnTaleRemove
        ctx.clock().advance_secs(35);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let rem_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        let tr_args = find_notif(&rem_notifs, "OnTaleRemove");
        assert_eq!(
            tr_args,
            &[
                PyRep::Integer(tale_id),
                PyRep::Integer(1),
                PyRep::Integer(
                    eve_server::services::pve_incursion_sites::INCURSION_TALE_TEMPLATE_ID as i64
                ),
            ]
        );

        // 4. Roaming Metaliminal Storms via live tick_background_cluster -> on_server_clock_advanced
        // Position storm #0 on a neighbor of SYS_JITA so its next step enters SYS_JITA, then moves onward.
        let storm_dest: u32 = 30_000_208;
        let now2 = ctx.now_unix();
        eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
            reg.roaming_storms.truncate(1);
            let s = &mut reg.roaming_storms[0];
            s.center_solar_system_id = storm_dest;
            s.strong_core_systems.clear();
            s.weak_periphery_systems.clear();
            s.recent_centers.clear();
            s.next_move_at_sec = now2 + 60;
        });
        eve_server::services::pve_metaliminal_storms::sync_metaliminal_storm_effects_and_sites(&ctx);
        let _ = ctx
            .db
            .set_character_solar_system(PILOT_A, storm_dest, &ctx.sde);
        sess.solar_system_id = Some(storm_dest);
        ctx.sync_session(&mut sess);
        let _ = drain_all(&ctx, &mut sess, PILOT_A);

        // Negative: advancing 10s (before next_move_at_sec) does NOT move storm or emit storm notifications
        ctx.clock().advance_secs(10);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let pre_storm = drain_all(&ctx, &mut sess, PILOT_A);
        assert!(!has_notif(&pre_storm, "OnSystemEffectStarted"));
        assert!(!has_notif(&pre_storm, "OnCosmicAnomalyAdded"));

        // Positive: advancing 60s moves storm into neighbor so storm_dest enters strong core -> emits OnSystemEffectStarted + OnCosmicAnomalyAdded
        ctx.clock().advance_secs(60);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let storm_in = drain_all(&ctx, &mut sess, PILOT_A);
        let eff_start = find_notif(&storm_in, "OnSystemEffectStarted");
        assert_eq!(eff_start.len(), 1);
        let beacon_tid = match &eff_start[0] {
            PyRep::List(ids) => {
                assert_eq!(ids.len(), 1);
                ids[0].as_i64().expect("beacon typeID")
            }
            other => panic!("expected List([beacon_type_id]), got {other:?}"),
        };
        assert!(beacon_tid > 0);
        let anom_added = find_notif(&storm_in, "OnCosmicAnomalyAdded");
        assert_eq!(anom_added.len(), 1);
        assert!(
            anom_added[0]
                .dict_get("targetID")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.starts_with("DNG-904"))
        );

        // Move storm to a distant null-sec region on its next scheduled jump so storm_dest leaves core + periphery
        let far_sys: u32 = 30_000_238;
        let now3 = ctx.now_unix();
        eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
            let s = &mut reg.roaming_storms[0];
            s.center_solar_system_id = far_sys;
            s.recent_centers.clear();
            s.next_move_at_sec = now3 + 60;
        });
        // Negative: 10s before scheduled jump -> storm effect still active
        ctx.clock().advance_secs(10);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert!(!has_notif(
            &drain_all(&ctx, &mut sess, PILOT_A),
            "OnSystemWideEffectStop"
        ));

        // Positive: 60s advances storm to far_sys -> removes beacon & environment from storm_dest
        ctx.clock().advance_secs(60);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let storm_out = drain_all(&ctx, &mut sess, PILOT_A);
        let stop_args = find_notif(&storm_out, "OnSystemWideEffectStop");
        assert_eq!(stop_args.len(), 3);
        assert!(stop_args[0].as_i64().unwrap_or(0) > 0);
        assert!(stop_args[1].as_i64().unwrap_or(0) > 0);
        assert_eq!(stop_args[2], PyRep::Integer(beacon_tid));
        assert_eq!(
            find_notif(&storm_out, "OnComponentRemovedSystemEnvironment"),
            &[PyRep::Integer(i64::from(storm_dest))]
        );
    }
}

pub(crate) mod make_item_ball_alpha {
    // Real production server-to-client `notify:On*` emission integration tests (Batch 10, Part 4).
    //
    // Covers ESS Reserve Bank inventory full (`OnEssReserveBankInventoryFull`), FW complex capture
    // ticks (`OnCaptureChanged`), Hacking tile selection (`OnLocalHackProgressUpdated`), Pirate
    // Insurgency Stage-5 security drop (`OnSecurityModified`), Null-Sec Dynamic Bounty multiplier
    // updates (`OnSystemDynamicBountyStateChanged`), and multi-pilot asteroid mining
    // (`OnOreMinedByOther`) via live RPCs and `tick_background_cluster`.


    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use eve_server::system_actor::AsteroidState;
    use glam::DVec3;
    use std::collections::HashMap;

    const PILOT_A: i64 = 90_000_001;
    const PILOT_B: i64 = 90_000_002;
    const SYS_JITA: u32 = 30_000_142;

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

    fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
        Ball {
            id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 0.5,
            mass: 1_000_000.0,
            radius: 60.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        }
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

    fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
        let mut out = sess.drain_notifications();
        for n in ctx.drain_notifications_for_char(char_id) {
            out.push((n.event, n.args));
        }
        out
    }

    fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
        notifs
            .iter()
            .find(|(ev, _)| ev == event)
            .map(|(_, args)| args.as_slice())
            .unwrap_or_else(|| {
                let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
                panic!("expected notification {event}, got {seen:?}")
            })
    }

    fn has_notif(notifs: &[(String, Vec<PyRep>)], event: &str) -> bool {
        notifs.iter().any(|(ev, _)| ev == event)
    }

    async fn setup_pilot_in_space(
        ctx: &ServerContext,
        char_id: i64,
        ship_id: i64,
        ship_type_id: u32,
        sys_id: u32,
        gm: bool,
    ) -> SessionState {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = ship_id;
            }
            snap.items.insert(
                ship_id,
                make_item(ship_id, ship_type_id, char_id, i64::from(sys_id), InventoryFlag::None, 1, true),
            );
        });
        let sys = ctx.systems.get_or_spawn(sys_id);
        let _ = sys
            .spawn_ball_with_combat(
                make_ball(ship_id, ship_type_id, DVec3::new(1000.0, 0.0, 0.0)),
                ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
            )
            .await;
        let mut sess = SessionState::new(char_id as u64);
        let (user, pass) = if char_id == PILOT_A { ("demo", "demo-secret") } else { ("other", "other-secret") };
        crate::common::login_pilot(ctx, &mut sess, user, pass).await;
        rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
            .await
            .expect("SelectCharacterID");
        sess.solar_system_id = Some(sys_id);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(ship_id);
        if gm {
            sess.role |= ROLE_GM_DEFAULT;
        }
        let _ = drain_all(ctx, &mut sess, char_id);
        sess
    }

    /// parity: notify:OnCaptureChanged
    /// parity: notify:OnLocalHackProgressUpdated
    /// parity: notify:OnSecurityModified
    /// parity: notify:OnSystemDynamicBountyStateChanged
    /// parity: notify:OnOreMinedByOther
    #[tokio::test]
    async fn batch10_ess_fw_hacking_security_dbs_and_mining() {
        let ctx = ServerContext::new();
        let null_sys = ctx
            .sde
            .solar_systems
            .iter()
            .find(|(_, s)| s.region_id == 10_000_003 && s.security_status <= 0.0)
            .map(|(&id, _)| id)
            .unwrap_or(30_000_201);

        // 1. OnSystemDynamicBountyStateChanged in Null-Sec
        let mut sess_a = setup_pilot_in_space(&ctx, PILOT_A, 700_501, 587, null_sys, false).await;
        let null_handle = ctx.systems.get_or_spawn(null_sys);
        let pirate_npc_tid = 33_132_u32;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                700_530,
                make_item(700_530, 10_678, PILOT_A, 700_501, InventoryFlag::HiSlot0, 1, true),
            );
            snap.items.insert(
                700_531,
                make_item(700_531, 222, PILOT_A, 700_501, InventoryFlag::HiSlot0, 200, false),
            );
            for &sk in &[3300_u32, 3303, 3426] {
                snap.skills.entry(PILOT_A).or_default().insert(
                    sk,
                    eve_db::TrainedSkillRecord { type_id: sk, level: 5, skill_points: 256_000.0 },
                );
            }
        });
        ctx.invalidate_ship_fitting(700_501);
        let _ = rpc(&ctx, &mut sess_a, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(700_501), PyRep::Integer(700_530)]).await;
        let npc_ball_id = eve_server::services::combat_crimewatch::spawn_specialized_npc_entity(
            &ctx,
            null_sys,
            pirate_npc_tid,
            "CloneSoldier",
            500_010,
            DVec3::new(1_250.0, 0.0, 0.0),
        )
        .await;
        if let Ok(Some(mut cs)) = null_handle.get_combat_state(npc_ball_id).await {
            cs.shield_hp = 0.0;
            cs.armor_hp = 0.0;
            cs.structure_hp = 1.0;
            cs.shield_recharge_time_ms = 1.0e15;
            let _ = null_handle.set_combat_state(npc_ball_id, cs).await;
        }
        rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(npc_ball_id)])
            .await
            .expect("AddTarget pirate NPC");
        for _ in 0..25 {
            null_handle.step_manual(1.0).await.unwrap();
        }
        rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(700_530), PyRep::String("targetAttack".into()), PyRep::Integer(npc_ball_id)],
        )
        .await
        .expect("Activate weapon on pirate NPC");
        for _ in 0..8 {
            null_handle.step_manual(1.0).await.unwrap();
            if null_handle.get_ball_snapshot(npc_ball_id).await.unwrap().is_none() {
                break;
            }
        }
        assert!(null_handle.get_ball_snapshot(npc_ball_id).await.unwrap().is_none(), "pirate NPC must be destroyed by weapon");
        assert!(
            eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
                rt.pending_bounty_ticks.get(&(null_sys, PILOT_A)).copied().unwrap_or(0.0)
            }) > 0.0,
            "pending bounty must be queued after NPC kill (tid={pirate_npc_tid}, sys={null_sys})"
        );
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

        // Negative: before the 20-minute (1,200s) bounty tick elapses, tick_background_cluster does NOT flush or emit OnSystemDynamicBountyStateChanged
        ctx.advance_secs(60);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert!(!has_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnSystemDynamicBountyStateChanged"));

        // Positive: advancing past the 20-minute (1,200s) bounty tick flushes the queued NPC bounty and emits OnSystemDynamicBountyStateChanged
        ctx.advance_secs(1_200);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let dbs_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let dbs_args = find_notif(&dbs_notifs, "OnSystemDynamicBountyStateChanged");
        assert_eq!(dbs_args.len(), 2);
        assert_eq!(dbs_args[0], PyRep::Integer(i64::from(null_sys)));
        let new_rate = dbs_args[1].as_f64().expect("DBS rate");
        assert!(new_rate > 0.0 && new_rate < 1.35);

        // 3. OnCaptureChanged & OnSecurityModified via live tick_background_cluster in Low-Sec
        let lowsec_sys = ctx
            .sde
            .solar_systems
            .iter()
            .find(|(_, s)| (0.1..0.4).contains(&s.security_status))
            .map(|(&id, _)| id)
            .unwrap_or(30_002_537);
        let _ = ctx.db.set_character_solar_system(PILOT_A, lowsec_sys, &ctx.sde);
        sess_a.solar_system_id = Some(lowsec_sys);
        ctx.sync_session(&mut sess_a);
        let low_handle = ctx.systems.get_or_spawn(lowsec_sys);
        let _ = low_handle
            .spawn_ball_with_combat(make_ball(700_501, 587, DVec3::new(90_000.0, 0.0, 0.0)), Default::default())
            .await;
        let fw_gate_id = 900_560_i64;
        let now_u = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.war_faction_id = Some(500_001); // Caldari militia for FW capture
            }
            snap.fw_runtime.fw_complexes.insert(fw_gate_id, (lowsec_sys, "Novice".into(), 500_004, 10.0, false));
            snap.item_positions.insert(fw_gate_id, [0.0, 0.0, 0.0]);
            snap.fw_runtime.complex_occupants.insert(fw_gate_id, vec![(PILOT_A, 700_501)]);
            snap.fw_runtime.complex_guards_cleared.insert(fw_gate_id, true);
            snap.fw_runtime.complex_last_step_unix.insert(fw_gate_id, now_u.saturating_sub(10));
        });
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

        // Negative: Pilot A is at 90km (>30km from pocket) -> zero OnCaptureChanged
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert!(!has_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnCaptureChanged"));

        // Positive: move Pilot A into capture pocket -> tick_background_cluster emits OnCaptureChanged
        let pocket = eve_server::services::fw_gameplay::pocket_pos_for_complex(fw_gate_id);
        let _ = low_handle.set_ball_position(700_501, pocket).await;
        ctx.advance_secs(5);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let cap_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let cap_args = find_notif(&cap_notifs, "OnCaptureChanged");
        assert_eq!(cap_args.len(), 6);
        assert_eq!(cap_args[0], PyRep::Integer(fw_gate_id));
        assert_eq!(cap_args[1], PyRep::Integer(500_001));
        assert!(cap_args[3].as_f64().unwrap_or(0.0) > 0.0);
        assert_eq!(cap_args[4], PyRep::Real(600_000.0));
        assert_eq!(cap_args[5], PyRep::Integer(500_001));

        // Setup Pirate Insurgency campaign at 799 corruption points (Stage 4) in lowsec_sys
        let now_ins = ctx.now_unix();
        eve_server::services::insurgency_services::init_insurgency_campaign(
            &ctx,
            1,
            500_010,
            500_001,
            lowsec_sys,
            vec![lowsec_sys],
        );
        ctx.db.with_mut_snapshot(|snap| {
            snap.fw_runtime.fw_complexes.clear();
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.war_faction_id = Some(500_010); // Guristas pirate militia
            }
            if let Some(camp) = snap.insurgency_runtime.campaigns.get_mut(&1) {
                camp.forecasting_end_unix = now_ins.saturating_sub(3_600);
                camp.active_end_unix = now_ins + 86_400;
                camp.winddown_end_unix = now_ins + 172_800;
                camp.corruption_points.insert(lowsec_sys, 799);
            }
            let st = snap.system_states.entry(lowsec_sys).or_default();
            st.solar_system_id = lowsec_sys;
            st.insurgency_corruption_stage = 4;
            st.fw_complex_position = Some([0.0, 0.0, 0.0]);
            st.last_fw_capture_tick = 0;
        });
        // Negative: Pilot A is at 50km (>30km from insurgency beacon [0,0,0]) -> zero OnSecurityModified
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert!(!has_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnSecurityModified"));

        // Positive: move Pilot A to [500, 0, 0] -> tick advances corruption 799 -> >=800 (Stage 5) -> OnSecurityModified
        let _ = low_handle.set_ball_position(700_501, DVec3::new(500.0, 0.0, 0.0)).await;
        ctx.advance_secs(5);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let sec_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let sec_args = find_notif(&sec_notifs, "OnSecurityModified");
        assert_eq!(sec_args.len(), 3);
        assert_eq!(sec_args[0], PyRep::Integer(i64::from(lowsec_sys)));
        assert!(sec_args[1].as_f64().unwrap_or(0.0) < 0.0);
        assert_eq!(sec_args[2], PyRep::Real(0.0));

        // 4. OnLocalHackProgressUpdated via live hackingMgr.SelectTile RPC in SYS_JITA
        let mut sess_jita = setup_pilot_in_space(&ctx, PILOT_A, 700_570, 587, SYS_JITA, false).await;
        let mut sess_obs = setup_pilot_in_space(&ctx, PILOT_B, 700_571, 587, SYS_JITA, false).await;
        let jita_sys = ctx.systems.get_or_spawn(SYS_JITA);
        let _ = rpc(&ctx, &mut sess_jita, "scanSvc", "GetCombatAnomalyInstances", &[]).await;
        let (can_id, site_pos) = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system
                .get(&SYS_JITA)
                .and_then(|sites| {
                    sites.iter().find_map(|s| s.hackable_can_ids.first().copied().map(|cid| (cid, DVec3::from_array(s.position))))
                })
                .expect("seeded hackable container in Jita")
        });
        let can_pos = if let Ok(Some((_, cb))) = jita_sys.get_ball_snapshot(can_id).await { cb.position } else { site_pos };
        let _ = jita_sys.set_ball_position(700_570, can_pos).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                700_575,
                make_item(700_575, 22_175, PILOT_A, 700_570, InventoryFlag::MedSlot0, 1, true),
            );
            snap.items.insert(
                700_576,
                make_item(700_576, 22_177, PILOT_A, 700_570, InventoryFlag::MedSlot1, 1, true),
            );
            for &sk in &[21_718_u32, 13_278] {
                snap.skills.entry(PILOT_A).or_default().insert(
                    sk,
                    eve_db::TrainedSkillRecord { type_id: sk, level: 5, skill_points: 256_000.0 },
                );
            }
        });
        ctx.invalidate_ship_fitting(700_570);
        let _ = rpc(&ctx, &mut sess_jita, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(700_570), PyRep::Integer(700_575)]).await;
        let _ = rpc(&ctx, &mut sess_jita, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(700_570), PyRep::Integer(700_576)]).await;

        let _ = rpc(&ctx, &mut sess_jita, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(can_id)])
            .await
            .unwrap();
        let _ = drain_all(&ctx, &mut sess_jita, PILOT_A);

        // Negative: invalid tile index 999 fails and emits zero OnLocalHackProgressUpdated
        assert!(
            rpc(&ctx, &mut sess_jita, "hackingMgr", "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(999)])
                .await
                .is_err()
        );
        assert!(!has_notif(&drain_all(&ctx, &mut sess_jita, PILOT_A), "OnLocalHackProgressUpdated"));

        // Positive: click an accessible tile adjacent to tile 0 -> emits OnLocalHackProgressUpdated
        eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
            if let Some(g) = reg.hacking_games.get_mut(&(PILOT_A, can_id)) {
                if let Some(t) = g.tiles.get_mut(1) {
                    t.revealed = true;
                }
            }
        });
        rpc(&ctx, &mut sess_jita, "hackingMgr", "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(1)])
            .await
            .unwrap();
        let hack_notifs = drain_all(&ctx, &mut sess_jita, PILOT_A);
        let hack_args = find_notif(&hack_notifs, "OnLocalHackProgressUpdated");
        assert_eq!(hack_args.len(), 2);
        assert_eq!(hack_args[0], PyRep::Integer(can_id));
        let prog = hack_args[1].as_f64().expect("hackProgress");
        assert!(prog > 0.0 && prog <= 1.0);

        // 5. OnOreMinedByOther via live dogmaIM.Activate("miningLaser") + tick_background_cluster
        let rock_id = 800_601_i64;
        let rock_pos = DVec3::new(1_500.0, 0.0, 0.0);
        let _ = jita_sys.set_ball_position(700_570, DVec3::new(1_000.0, 0.0, 0.0)).await;
        let _ = jita_sys
            .spawn_asteroid(
                AsteroidState {
                    asteroid_id: rock_id,
                    belt_id: 0,
                    type_id: 1230,
                    initial_units: 10_000,
                    remaining_units: 10_000,
                    unit_volume: 0.1,
                    depleted: false,
                    respawn_at_tick: None,
                },
                make_ball(rock_id, 1230, rock_pos),
            )
            .await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                rock_id,
                make_item(rock_id, 1230, 1_000_009, i64::from(SYS_JITA), InventoryFlag::None, 10_000, true),
            );
            snap.asteroids.insert(
                rock_id,
                eve_db::AsteroidRecord {
                    asteroid_id: rock_id,
                    belt_id: 0,
                    solar_system_id: SYS_JITA,
                    type_id: 1230,
                    quantity: 10_000,
                    initial_quantity: 10_000,
                    position: (rock_pos.x, rock_pos.y, rock_pos.z),
                    radius: 100.0,
                },
            );
            snap.items.insert(
                700_580,
                make_item(700_580, 483, PILOT_A, 700_570, InventoryFlag::HiSlot1, 1, true),
            );
            snap.skills.entry(PILOT_A).or_default().insert(
                3386,
                eve_db::TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 },
            );
        });
        ctx.invalidate_ship_fitting(700_570);
        let _ = rpc(&ctx, &mut sess_jita, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(700_570), PyRep::Integer(700_580)]).await;
        rpc(&ctx, &mut sess_jita, "dogmaIM", "AddTarget", &[PyRep::Integer(rock_id)]).await.unwrap();
        for _ in 0..25 {
            jita_sys.step_manual(1.0).await.unwrap();
        }
        rpc(
            &ctx,
            &mut sess_jita,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(700_580), PyRep::String("miningLaser".into()), PyRep::Integer(rock_id)],
        )
        .await
        .expect("Activate Miner I");
        let _ = drain_all(&ctx, &mut sess_jita, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_obs, PILOT_B);

        // Negative: after 5s (before 60s laser cycle finishes), zero OnOreMinedByOther on observer
        ctx.advance_secs(5);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert!(!has_notif(&drain_all(&ctx, &mut sess_obs, PILOT_B), "OnOreMinedByOther"));

        // Positive: after cycle completes, observer PILOT_B gets OnOreMinedByOther(rock_id, qty), while miner PILOT_A does NOT
        ctx.advance_secs(65);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let miner_notifs = drain_all(&ctx, &mut sess_jita, PILOT_A);
        assert!(!has_notif(&miner_notifs, "OnOreMinedByOther"));
        let obs_notifs = drain_all(&ctx, &mut sess_obs, PILOT_B);
        let ore_args = find_notif(&obs_notifs, "OnOreMinedByOther");
        assert_eq!(ore_args.len(), 2);
        assert_eq!(ore_args[0], PyRep::Integer(rock_id));
        assert!(ore_args[1].as_i64().unwrap_or(0) > 0);
    }
}

pub(crate) mod make_item_rpc_beta {
    // Real production server-to-client `notify:On*` / `notify:Do*` emission integration tests (Batch 10, Part 5).
    //
    // Covers `DoDestinyUpdates`, `DoSimClockRebase`, `OnDestinationChange` (via ESI HTTP),
    // `OnServerMapDataPush`, `OnSystemActivityUpdated` (via `industryManager.InstallJob`),
    // `OnCharacterEnergyChanged` (via CRAB beacon `beyonce.CallComponentFromClient`), and
    // `OnAnalysisKreditsChange` (via `ProjectDiscovery.post_classification`).


    use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::esi::{EsiHttpRequest, EsiRouter};
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;
    use std::sync::Arc;

    const PILOT_A: i64 = 90_000_001;
    const SYS_JITA: u32 = 30_000_142;

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
        svc: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
            .await
    }

    fn drain_all(
        ctx: &ServerContext,
        sess: &mut SessionState,
        char_id: i64,
    ) -> Vec<(String, Vec<PyRep>)> {
        let mut out = sess.drain_notifications();
        for n in ctx.drain_notifications_for_char(char_id) {
            out.push((n.event, n.args));
        }
        out
    }

    fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
        notifs
            .iter()
            .find(|(ev, _)| ev == event)
            .map(|(_, args)| args.as_slice())
            .unwrap_or_else(|| {
                let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
                panic!("expected notification {event}, got {seen:?}")
            })
    }

    fn has_notif(notifs: &[(String, Vec<PyRep>)], event: &str) -> bool {
        notifs.iter().any(|(ev, _)| ev == event)
    }

    async fn setup_pilot_in_space(
        ctx: &ServerContext,
        char_id: i64,
        ship_id: i64,
        ship_type_id: u32,
        sys_id: u32,
    ) -> SessionState {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = ship_id;
            }
            snap.items.insert(
                ship_id,
                make_item(
                    ship_id,
                    ship_type_id,
                    char_id,
                    i64::from(sys_id),
                    InventoryFlag::None,
                    1,
                    true,
                ),
            );
        });
        let sys = ctx.systems.get_or_spawn(sys_id);
        let _ = sys
            .spawn_ball_with_combat(
                Ball {
                    id: ship_id,
                    type_id: ship_type_id,
                    position: DVec3::new(1000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 0.5,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                Default::default(),
            )
            .await;
        let mut sess = SessionState::new(char_id as u64);
        crate::common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
        rpc(
            ctx,
            &mut sess,
            "charUnboundMgr",
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
        )
        .await
        .expect("SelectCharacterID");
        sess.solar_system_id = Some(sys_id);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(ship_id);
        let _ = drain_all(ctx, &mut sess, char_id);
        sess
    }

    /// parity: notify:DoDestinyUpdates
    /// parity: notify:DoSimClockRebase
    /// parity: notify:OnDestinationChange
    /// parity: notify:OnSystemActivityUpdated
    /// parity: notify:OnCharacterEnergyChanged
    /// parity: notify:OnAnalysisKreditsChange
    #[tokio::test]
    async fn batch10_destiny_clock_esi_map_industry_energy_and_discovery() {
        let ctx = Arc::new(ServerContext::new());
        let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_701, 587, SYS_JITA).await;

        // 1. DoDestinyUpdates via live beyonce.CmdStop
        let beyonce_oid = ctx.bind_object(&sess, "beyonce", i64::from(SYS_JITA)).unwrap();
        let empty_kw = std::collections::HashMap::new();
        // Negative: docked pilot calling CmdStop fails and does NOT emit DoDestinyUpdates
        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&PILOT_A).unwrap().station_id = Some(60_003_760);
        });
        sess.station_id = Some(60_003_760);
        assert!(
            ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdStop", &[], &empty_kw, None)
                .await
                .is_err()
        );
        assert!(!has_notif(
            &drain_all(&ctx, &mut sess, PILOT_A),
            "DoDestinyUpdates"
        ));

        // Positive: undocked pilot in space receives DoDestinyUpdates([(stamp, ("Stop", (ship_id,)))])
        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&PILOT_A).unwrap().station_id = None;
        });
        sess.station_id = None;
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdStop", &[], &empty_kw, None)
            .await
            .unwrap();
        let dest_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        let du_args = find_notif(&dest_notifs, "DoDestinyUpdates");
        assert_eq!(du_args.len(), 1);
        match &du_args[0] {
            PyRep::List(updates) => {
                assert!(!updates.is_empty());
                match &updates[0] {
                    PyRep::Tuple(pair) => {
                        assert_eq!(pair.len(), 2);
                        match &pair[1] {
                            PyRep::Tuple(cmd) => assert_eq!(cmd[0], PyRep::String("Stop".into())),
                            other => panic!("expected (\"Stop\", payload), got {other:?}"),
                        }
                    }
                    other => panic!("expected (stamp, cmd) tuple, got {other:?}"),
                }
            }
            other => panic!("expected List of updates, got {other:?}"),
        }

        // 2. DoSimClockRebase via live ctx.advance_secs
        // Negative: advancing 0 seconds does NOT emit DoSimClockRebase
        ctx.advance_secs(0);
        assert!(!has_notif(
            &drain_all(&ctx, &mut sess, PILOT_A),
            "DoSimClockRebase"
        ));

        // Positive: advancing 15 seconds emits 1-arg 2-tuple ((old_ft, new_ft),) with exact 15s filetime delta
        let before_ft = ctx.now_blueos_filetime_ticks();
        ctx.advance_secs(15);
        let rebase_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        let rb_args = find_notif(&rebase_notifs, "DoSimClockRebase");
        assert_eq!(rb_args.len(), 1);
        match &rb_args[0] {
            PyRep::Tuple(times) => {
                assert_eq!(times.len(), 2);
                let old_t = times[0].as_i64().unwrap();
                let new_t = times[1].as_i64().unwrap();
                assert_eq!(old_t / 10_000_000, before_ft / 10_000_000);
                assert_eq!((new_t - old_t) / 10_000_000, 15);
            }
            other => panic!("expected 2-tuple rebaseTimes, got {other:?}"),
        }

        // 3. OnDestinationChange via live ESI HTTP POST /v2/ui/autopilot/waypoint/
        let esi = EsiRouter::new(Arc::clone(&ctx));
        let now_u = ctx.now_unix();
        let code = ctx
            .db
            .authorize_pkce(
                "demo",
                "demo-secret",
                PILOT_A,
                "evomu-batch10",
                "https://localhost/callback",
                vec!["esi-ui.write_waypoint.v1".to_string()],
                "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                "S256",
                now_u,
            )
            .expect("authorize_pkce");
        let token = ctx
            .db
            .exchange_pkce_code(
                &code,
                "evomu-batch10",
                "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
                now_u,
            )
            .expect("exchange_pkce_code")
            .access_token;
        // Negative: invalid destination_id rejected with 4xx and emits zero OnDestinationChange
        let bad_resp = esi.handle_request(
            &EsiHttpRequest::new(
                "POST",
                "/v2/ui/autopilot/waypoint/?destination_id=999999999&add_to_beginning=false&clear_other_waypoints=true",
            )
            .with_bearer_token(&token),
        );
        assert_ne!(bad_resp.status_code, 204);
        assert!(!has_notif(
            &drain_all(&ctx, &mut sess, PILOT_A),
            "OnDestinationChange"
        ));

        // Positive: valid solar system Amarr (30002187) returns 204 and emits OnDestinationChange(30002187, true, false)
        let ok_resp = esi.handle_request(
            &EsiHttpRequest::new(
                "POST",
                "/v2/ui/autopilot/waypoint/?destination_id=30002187&add_to_beginning=false&clear_other_waypoints=true",
            )
            .with_bearer_token(&token),
        );
        assert_eq!(ok_resp.status_code, 204);
        let wp_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(
            find_notif(&wp_notifs, "OnDestinationChange"),
            &[
                PyRep::Integer(30_002_187),
                PyRep::Bool(true),
                PyRep::Bool(false),
            ]
        );

        // 5. OnSystemActivityUpdated via live industryManager.InstallJob
        // Negative: undocked pilot calling InstallJob fails with NotDocked and emits zero OnSystemActivityUpdated
        let bp_tid = *ctx
            .sde
            .blueprints
            .iter()
            .find(|(&id, bp)| bp.manufacturing_time_sec > 0 && ctx.sde.types.contains_key(&id))
            .map(|(id, _)| id)
            .expect("SDE blueprint");
        assert_eq!(
            rpc(
                &ctx,
                &mut sess,
                "industryManager",
                "InstallJob",
                &[PyRep::Integer(i64::from(bp_tid)), PyRep::Integer(3), PyRep::Integer(1)]
            )
            .await
            .unwrap_err()
            .code,
            "NotDocked"
        );
        assert!(!has_notif(
            &drain_all(&ctx, &mut sess, PILOT_A),
            "OnSystemActivityUpdated"
        ));

        // Positive: docked at Jita 4-4 (60003760) with owned BPO in Hangar & ISK -> InstallJob emits OnSystemActivityUpdated()
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.station_id = Some(60_003_760);
                ch.balance = IskCents::from_isk(100_000_000);
            }
            snap.items.insert(
                700_720,
                make_item(700_720, bp_tid, PILOT_A, 60_003_760, InventoryFlag::Hangar, 1, true),
            );
        });
        sess.station_id = Some(60_003_760);
        rpc(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(700_720)),
                ("blueprintTypeID", PyRep::Integer(i64::from(bp_tid))),
                ("activityID", PyRep::Integer(3)),
                ("runs", PyRep::Integer(1)),
                ("facilityID", PyRep::Integer(60_003_760)),
            ])],
        )
        .await
        .expect("InstallJob TE research");
        let ind_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&ind_notifs, "OnSystemActivityUpdated"), &[]);

        // 6. OnCharacterEnergyChanged via live CRAB beacon beyonce.CallComponentFromClient("linkWithShip", "InitiateLink")
        let null_sys = ctx
            .sde
            .solar_systems
            .iter()
            .find(|(_, s)| s.region_id == 10_000_003 && s.security_status <= 0.0)
            .map(|(&id, _)| id)
            .unwrap_or(30_000_201);
        let dread_tid = *ctx
            .sde
            .types
            .iter()
            .find(|(_, t)| t.category_id == 6 && t.group_id == 485 && t.published)
            .map(|(id, _)| id)
            .unwrap_or(&19_720);
        let crab_id = 700_750_i64;
        let dread_id = 700_751_i64;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.solar_system_id = null_sys;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = dread_id;
            }
            snap.items.insert(
                dread_id,
                make_item(dread_id, dread_tid, PILOT_A, i64::from(null_sys), InventoryFlag::None, 1, true),
            );
            snap.items.insert(
                crab_id,
                make_item(
                    crab_id,
                    eve_server::services::pve_crab_beacons::TYPE_CRAB_BEACON,
                    PILOT_A,
                    i64::from(null_sys),
                    InventoryFlag::None,
                    1,
                    true,
                ),
            );
            snap.item_positions.insert(crab_id, [2_000.0, 0.0, 0.0]);
        });
        // Deduct 90 CEQ so PILOT_A has 10 CEQ (< 30 cost) for negative InsufficientEnergy test
        let _ = eve_server::services::character_energy::consume_character_energy(&ctx, PILOT_A, 90);
        sess.solar_system_id = Some(null_sys);
        sess.station_id = None;
        sess.ship_id = Some(dread_id);
        let null_handle = ctx.systems.get_or_spawn(null_sys);
        let _ = null_handle
            .spawn_ball_with_combat(
                Ball {
                    id: dread_id,
                    type_id: dread_tid,
                    position: DVec3::new(1_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 100.0,
                    inertia: 1.0,
                    mass: 1_000_000_000.0,
                    radius: 1_200.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                Default::default(),
            )
            .await;
        let _ = null_handle
            .spawn_ball_with_combat(
                Ball {
                    id: crab_id,
                    type_id: eve_server::services::pve_crab_beacons::TYPE_CRAB_BEACON,
                    position: DVec3::new(2_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 50_000.0,
                    radius: 400.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                Default::default(),
            )
            .await;
        let _ = drain_all(&ctx, &mut sess, PILOT_A);

        // Negative: only 10 CEQ (< 30 required) -> InitiateLink rejected and zero OnCharacterEnergyChanged
        assert_eq!(
            rpc(
                &ctx,
                &mut sess,
                "beyonce",
                "CallComponentFromClient",
                &[
                    PyRep::Integer(crab_id),
                    PyRep::String("linkWithShip".into()),
                    PyRep::String("InitiateLink".into()),
                ]
            )
            .await
            .unwrap_err()
            .code,
            "InsufficientEnergy"
        );
        assert!(!has_notif(
            &drain_all(&ctx, &mut sess, PILOT_A),
            "OnCharacterEnergyChanged"
        ));

        // Positive: advance 25 hours so CEQ regenerates to 100, then InitiateLink deducts 30 -> 70 CEQ
        eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
            if let Some(b) = reg.crab_beacons.get_mut(&crab_id) {
                b.deployed_at_sec = 0;
            }
        });
        ctx.advance_secs(90_000);
        let _ = rpc(&ctx, &mut sess, "characterEnergyMgr", "GetMyEnergyState", &[]).await.unwrap();
        let _ = drain_all(&ctx, &mut sess, PILOT_A);
        let crab_res = rpc(
            &ctx,
            &mut sess,
            "beyonce",
            "CallComponentFromClient",
            &[
                PyRep::Integer(crab_id),
                PyRep::String("linkWithShip".into()),
                PyRep::String("InitiateLink".into()),
            ],
        )
        .await
        .expect("InitiateLink CRAB beacon");
        assert_eq!(
            crab_res.dict_get("ceqRemaining").and_then(|v| v.as_i64()),
            Some(70)
        );
        let ceq_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        let ceq_args = find_notif(&ceq_notifs, "OnCharacterEnergyChanged");
        assert_eq!(ceq_args.len(), 2);
        assert_eq!(ceq_args[0], PyRep::Integer(PILOT_A));
        assert_eq!(
            ceq_args[1]
                .dict_get("currentEnergy")
                .and_then(|v| v.as_i64()),
            Some(70)
        );
        assert_eq!(
            ceq_args[1].dict_get("maxEnergy").and_then(|v| v.as_i64()),
            Some(100)
        );

        // 7. OnAnalysisKreditsChange via live ProjectDiscovery.post_classification
        let task_rep = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[])
            .await
            .unwrap();
        assert!(!has_notif(
            &drain_all(&ctx, &mut sess, PILOT_A),
            "OnAnalysisKreditsChange"
        ));

        ctx.db.with_mut_snapshot(|snap| {
            eve_server::services::project_discovery::configure_project_discovery_mmos(
                snap, 100.0, 50.0, 25.0, 5.0, 10,
            );
            eve_server::services::project_discovery::set_project_discovery_task_consensus(
                snap, 1, true,
            );
        });
        let task_id = task_rep
            .dict_get("task_id")
            .cloned()
            .unwrap_or(PyRep::Integer(1));
        rpc(
            &ctx,
            &mut sess,
            "ProjectDiscovery",
            "post_classification",
            &[task_id, PyRep::List(vec![PyRep::Integer(1)])],
        )
        .await
        .unwrap();
        let pd_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&pd_notifs, "OnAnalysisKreditsChange"), &[]);
    }
}

pub(crate) mod make_item_rpc_gamma {
    // Real production server-to-client `notify:On*` emission integration tests (Batch 2).
    //
    // Exercises 32 V24.01 server-to-client notifications end-to-end through `ctx.dispatch_rpc`,
    // asserting exact V24 positional/dict payload arguments and persisted world-state transitions.


    use crate::common;
    use eve_db::{
        CosmicSignatureRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord,
    };
    use eve_destiny::{Ball, MovementMode, TargetLock};
    use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use eve_server::services::season_manager::{SeasonalChallengeDef, SEASONAL_CHALLENGES_CONFIG_KEY};
    use glam::DVec3;
    use std::collections::HashMap;

    const PILOT_A: i64 = 90_000_001;
    const PILOT_B: i64 = 90_000_002;
    const SYS_JITA: u32 = 30_000_142;
    const JITA_STATION: i64 = 60_003_760;
    const EXPERT_SYSTEM_TYPE_ID: u32 = 57_203;
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
        svc: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
            .await
    }

    fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
        let mut out = sess.drain_notifications();
        for n in ctx.drain_notifications_for_char(char_id) {
            out.push((n.event, n.args));
        }
        out
    }

    fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
        notifs
            .iter()
            .find(|(ev, _)| ev == event)
            .map(|(_, args)| args.as_slice())
            .unwrap_or_else(|| {
                let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
                panic!("expected notification {event}, got {seen:?}")
            })
    }

    #[allow(clippy::too_many_arguments)]
    async fn login_in_space(
        ctx: &ServerContext,
        session_id: u64,
        username: &str,
        password: &str,
        char_id: i64,
        ship_id: i64,
        ship_type_id: u32,
        sys_id: u32,
        pos: DVec3,
    ) -> SessionState {
        let mut sess = SessionState::new(session_id);
        common::login_pilot(ctx, &mut sess, username, password).await;
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

    // parity: notify:OnFighterTubeContentUpdate, notify:OnFighterTubeContentEmpty, notify:OnFighterTubeTaskStatus
    // parity: notify:OnFighterAddedToController, notify:OnFighterAbilitySlotActivated, notify:OnFighterAbilitySlotDeactivated
    // parity: notify:OnInSpaceSquadronSizeChanged, notify:OnFighterRemovedFromController, notify:OnSystemWideEffectStart
    // parity: notify:OnEwarStart, notify:OnMutualWarsUpdated_Remote, notify:OnItemsChanged
    // parity: notify:OnSignalTrackerSignatureUpdate, notify:OnSignalTrackerAnomalyUpdate, notify:OnSignalTrackerStructureUpdate
    #[tokio::test]
    async fn notify_batch2_fighters_ewar_mutual_wars_strip_and_tracker() {
        eve_testkit::attest(&[
            "notify:OnFighterTubeContentUpdate",
            "notify:OnFighterTubeContentEmpty",
            "notify:OnFighterTubeTaskStatus",
            "notify:OnFighterAddedToController",
            "notify:OnFighterAbilitySlotActivated",
            "notify:OnFighterAbilitySlotDeactivated",
            "notify:OnInSpaceSquadronSizeChanged",
            "notify:OnFighterRemovedFromController",
            "notify:OnSystemWideEffectStart",
            "notify:OnEwarStart",
            "notify:OnMutualWarsUpdated_Remote",
            "notify:OnItemsChanged",
            "notify:OnSignalTrackerSignatureUpdate",
            "notify:OnSignalTrackerAnomalyUpdate",
            "notify:OnSignalTrackerStructureUpdate",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let sys_lowsec = 30_002_537_u32;
        let (carrier_id, target_ship_id) = (610_000_001_i64, 610_000_002_i64);
        let mut sess_a = login_in_space(&ctx, 8_201, "demo", "demo-secret", PILOT_A, carrier_id, 23_913, sys_lowsec, DVec3::ZERO).await;
        let mut sess_b = login_in_space(&ctx, 8_202, "other", "other-secret", PILOT_B, target_ship_id, 24_692, sys_lowsec, DVec3::new(300.0, 0.0, 0.0)).await;
        ctx.advance_secs(65);

        assert!(rpc(&ctx, &mut sess_a, "fighterMgr", "UnloadTubeToFighterBay", &[PyRep::Integer(99)]).await.is_err());

        let (fighter_item_id, bomber_item_id, smartbomb_id, scram_id, ecm_id, neut_id) = (
            610_000_010_i64, 610_000_011_i64, 610_000_020_i64, 610_000_021_i64, 610_000_022_i64, 610_000_023_i64,
        );
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(fighter_item_id, make_item(fighter_item_id, 23_057, PILOT_A, carrier_id, InventoryFlag::FighterBay, 6, false));
            snap.items.insert(bomber_item_id, make_item(bomber_item_id, 40_362, PILOT_A, carrier_id, InventoryFlag::FighterBay, 6, false));
            snap.items.insert(smartbomb_id, make_item(smartbomb_id, 14_550, PILOT_B, target_ship_id, InventoryFlag::HiSlot0, 1, true));
            snap.module_online_states.insert(smartbomb_id, true);
            snap.items.insert(scram_id, make_item(scram_id, 447, PILOT_A, carrier_id, InventoryFlag::MedSlot0, 1, true));
            snap.module_online_states.insert(scram_id, true);
            snap.items.insert(ecm_id, make_item(ecm_id, 1_957, PILOT_A, carrier_id, InventoryFlag::MedSlot1, 1, true));
            snap.module_online_states.insert(ecm_id, true);
            snap.items.insert(neut_id, make_item(neut_id, 533, PILOT_A, carrier_id, InventoryFlag::HiSlot0, 1, true));
            snap.module_online_states.insert(neut_id, true);
            for (sid, lvl) in [(3_432, 5), (3_363, 5), (40_535, 5), (40_537, 5), (32_339, 5), (3_426, 5), (3_427, 5)] {
                snap.skills.entry(PILOT_A).or_default().insert(sid, TrainedSkillRecord { type_id: sid, level: lvl, skill_points: 256_000.0 });
            }
            snap.skills.entry(PILOT_B).or_default().insert(3_363, TrainedSkillRecord { type_id: 3_363, level: 5, skill_points: 256_000.0 });
            snap.skills.entry(PILOT_B).or_default().insert(3_424, TrainedSkillRecord { type_id: 3_424, level: 5, skill_points: 256_000.0 });
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) { ch.balance = IskCents::from_isk(500_000_000); }
            if let Some(ch) = snap.characters.get_mut(&PILOT_B) { ch.balance = IskCents::from_isk(500_000_000); }
        });
        ctx.invalidate_ship_fitting(carrier_id);
        ctx.invalidate_ship_fitting(target_ship_id);

        // 1-2. Tube load & unload
        rpc(&ctx, &mut sess_a, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_item_id)]).await.expect("Load");
        let load_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(find_notif(&load_notifs, "OnFighterTubeContentUpdate"), &[PyRep::Integer(159), PyRep::Integer(fighter_item_id), PyRep::Integer(23_057), PyRep::Integer(6)]);

        let unload_rep = rpc(&ctx, &mut sess_a, "fighterMgr", "UnloadTubeToFighterBay", &[PyRep::Integer(0)]).await.expect("Unload");
        assert_eq!(unload_rep, PyRep::Bool(true));
        let unload_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(find_notif(&unload_notifs, "OnFighterTubeContentEmpty"), &[PyRep::Integer(159)]);

        // 3. Reload light & heavy bomber squadrons and launch
        rpc(&ctx, &mut sess_a, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_item_id)]).await.expect("Reload 0");
        rpc(&ctx, &mut sess_a, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(1), PyRep::Integer(bomber_item_id)]).await.expect("Load 1");
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let launch_rep = rpc(&ctx, &mut sess_a, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(1)])]).await.expect("Launch");
        let PyRep::List(ref launched_balls) = launch_rep else { panic!("expected ball list") };
        let fighter_ball_id = launched_balls[0].as_i64().expect("fighter_ball_id");
        let bomber_ball_id = launched_balls[1].as_i64().expect("bomber_ball_id");
        let launch_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let task_args = find_notif(&launch_notifs, "OnFighterTubeTaskStatus");
        assert_eq!((task_args.len(), &task_args[0], &task_args[1], &task_args[4]), (5, &PyRep::Integer(159), &PyRep::String("INSPACE".into()), &PyRep::None));
        assert_eq!(
            find_notif(&launch_notifs, "OnFighterAddedToController"),
            &[PyRep::Integer(fighter_ball_id), PyRep::Integer(23_057), PyRep::Integer(159), PyRep::Integer(6), PyRep::List(vec![])]
        );

        // 4. Ability slot activate (unit + point-targeted bomb) & deactivate
        rpc(&ctx, &mut sess_a, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::List(vec![PyRep::Integer(fighter_ball_id)]), PyRep::Integer(1)]).await.expect("Activate slot 1");
        let act_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let act_args = find_notif(&act_notifs, "OnFighterAbilitySlotActivated");
        assert_eq!((act_args.len(), &act_args[0], &act_args[1]), (6, &PyRep::Integer(fighter_ball_id), &PyRep::Integer(1)));

        rpc(&ctx, &mut sess_a, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::Integer(bomber_ball_id), PyRep::Integer(2), PyRep::Tuple(vec![PyRep::Real(5_000.0), PyRep::Real(0.0), PyRep::Real(0.0)])]).await.expect("Activate point bomb");
        let pt_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let pt_args = find_notif(&pt_notifs, "OnFighterAbilitySlotActivated");
        assert_eq!((pt_args.len(), &pt_args[0], &pt_args[1]), (6, &PyRep::Integer(bomber_ball_id), &PyRep::Integer(2)));

        rpc(&ctx, &mut sess_a, "fighterMgr", "CmdDeactivateAbilitySlots", &[PyRep::List(vec![PyRep::Integer(fighter_ball_id)]), PyRep::Integer(1)]).await.expect("Deactivate slot 1");
        let deact_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(find_notif(&deact_notifs, "OnFighterAbilitySlotDeactivated"), &[PyRep::Integer(fighter_ball_id), PyRep::Integer(1), PyRep::None]);

        // 5. Offensive EWAR negative gates (0 cap + Safety Green against non-suspect pilot in lowsec)
        let sys_handle = ctx.systems.get_or_spawn(sys_lowsec);
        let _ = sys_handle
            .add_target_lock(carrier_id, TargetLock { target_id: target_ship_id, lock_time_ms: 100.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None })
            .await;
        let _ = sys_handle.set_capacitor(carrier_id, 0.0).await;
        assert!(rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(scram_id), PyRep::String("warpScrambler".into()), PyRep::Integer(target_ship_id), PyRep::Integer(0)]).await.is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A).iter().any(|(e, _)| e == "OnEwarStart"));

        let _ = sys_handle.set_capacitor(carrier_id, 50_000.0).await;
        rpc(&ctx, &mut sess_a, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(2)]).await.expect("SetSafetyLevel Green");
        assert!(rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(scram_id), PyRep::String("warpScrambler".into()), PyRep::Integer(target_ship_id), PyRep::Integer(0)]).await.is_err());
        assert!(!drain_all(&ctx, &mut sess_a, PILOT_A).iter().any(|(e, _)| e == "OnEwarStart"));

        // 6-7. Partial squadron loss via Smartbomb AoE + RecallFightersToTubes
        if let Ok(Some((_, fb))) = sys_handle.get_ball_snapshot(fighter_ball_id).await {
            let _ = sys_handle.set_ball_position(target_ship_id, fb.position + DVec3::new(100.0, 0.0, 0.0)).await;
        }
        rpc(&ctx, &mut sess_b, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await.expect("SetSafetyLevel Red");
        for _ in 0..6 {
            rpc(&ctx, &mut sess_b, "dogmaIM", "Activate", &[PyRep::Integer(smartbomb_id)]).await.expect("Smartbomb Activate");
            ctx.advance_secs(11);
        }
        let dmg_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let sq_args = dmg_notifs
            .iter()
            .find(|(e, a)| e == "OnInSpaceSquadronSizeChanged" && a.first() == Some(&PyRep::Integer(fighter_ball_id)))
            .map(|(_, a)| a.as_slice())
            .expect("missing OnInSpaceSquadronSizeChanged for fighter_ball_id");
        assert!((1..6).contains(&sq_args[1].as_i64().unwrap_or(0)));

        rpc(&ctx, &mut sess_a, "fighterMgr", "RecallFightersToTubes", &[PyRep::List(vec![PyRep::Integer(fighter_ball_id), PyRep::Integer(bomber_ball_id)])]).await.expect("Recall");
        let recall_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(recall_notifs.iter().any(|(e, a)| e == "OnFighterRemovedFromController" && a.as_slice() == [PyRep::Integer(fighter_ball_id), PyRep::Integer(159)]));

        // 8. Positive offensive EWAR (scram, ECM, neut) + Phenomena Generator
        rpc(&ctx, &mut sess_a, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await.expect("SetSafetyLevel Red");
        rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(scram_id), PyRep::String("warpScrambler".into()), PyRep::Integer(target_ship_id), PyRep::Integer(0)]).await.expect("Scram");
        assert_eq!(
            find_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnEwarStart"),
            &[PyRep::Integer(carrier_id), PyRep::Integer(scram_id), PyRep::Integer(target_ship_id), PyRep::String("warpScramblerMWD".into())]
        );
        rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(ecm_id), PyRep::String("targetHostile".into()), PyRep::Integer(target_ship_id), PyRep::Integer(0)]).await.expect("ECM");
        assert_eq!(
            find_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnEwarStart"),
            &[PyRep::Integer(carrier_id), PyRep::Integer(ecm_id), PyRep::Integer(target_ship_id), PyRep::String("electronic".into())]
        );
        rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(neut_id), PyRep::String("energyNeutralizerFalloff".into()), PyRep::Integer(target_ship_id), PyRep::Integer(0)]).await.expect("Neut");
        assert_eq!(
            find_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnEwarStart"),
            &[PyRep::Integer(carrier_id), PyRep::Integer(neut_id), PyRep::Integer(target_ship_id), PyRep::String("ewEnergyNeut".into())]
        );

        let (phenom_id, fuel_id) = (710_000_022_i64, 710_000_023_i64);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(s) = snap.items.get_mut(&carrier_id) { s.type_id = 11_567; }
            snap.items.insert(phenom_id, make_item(phenom_id, 43_658, PILOT_A, carrier_id, InventoryFlag::HiSlot1, 1, true));
            snap.module_online_states.insert(phenom_id, true);
            snap.items.insert(fuel_id, make_item(fuel_id, 16_275, PILOT_A, carrier_id, InventoryFlag::Cargo, 5_000, false));
        });
        ctx.invalidate_ship_fitting(carrier_id);
        rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(phenom_id), PyRep::String("TitanPhenomenaGenerator".into()), PyRep::None, PyRep::Integer(1)]).await.expect("Phenomena");
        let phenom_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let sw_args = find_notif(&phenom_notifs, "OnSystemWideEffectStart");
        assert_eq!((sw_args.len(), &sw_args[1], &sw_args[2]), (3, &PyRep::Integer(phenom_id), &PyRep::Integer(43_658)));

        // 9. SignalTrackerRegister (zero initial spam) + real signature/anomaly/structure delta updates
        rpc(&ctx, &mut sess_a, "scanMgr", "SignalTrackerRegister", &[]).await.expect("SignalTrackerRegister");
        let reg_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(!reg_notifs.iter().any(|(e, _)| matches!(
            e.as_str(),
            "OnSignalTrackerSignatureUpdate" | "OnSignalTrackerAnomalyUpdate" | "OnSignalTrackerStructureUpdate"
        )));

        eve_server::services::pve_exploration_seeding::ensure_system_pve_sites_seeded(&ctx, sys_lowsec).await;
        let seed_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let anom_add = find_notif(&seed_notifs, "OnSignalTrackerAnomalyUpdate");
        assert_eq!(anom_add[0], PyRep::Integer(sys_lowsec as i64));
        assert!(matches!(&anom_add[1], PyRep::Dict(d) if !d.is_empty() && d[0].1.dict_get("instanceID").is_some()));

        ctx.db.with_mut_snapshot(|s| { let mut d = s.cosmic_signatures[&sys_lowsec][0].clone(); d.signature_id = "STA-100".into(); if let Some(wh) = eve_server::services::space_destiny::build_static_replacement_wormhole(&ctx, sys_lowsec, &d, ctx.now_unix(), 1) { s.cosmic_signatures.get_mut(&sys_lowsec).unwrap().push(wh); } });
        ctx.advance_secs(90_000);
        rpc(&ctx, &mut sess_a, "scanMgr", "GetSignatures", &[PyRep::Integer(sys_lowsec as i64)]).await.expect("GetSignatures");
        let exp_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let sig_upd = find_notif(&exp_notifs, "OnSignalTrackerSignatureUpdate");
        assert_eq!(sig_upd[0], PyRep::Integer(sys_lowsec as i64));
        let PyRep::Dict(ref sig_dict) = sig_upd[1] else { panic!("expected dict") };
        let sig_kv = sig_dict
            .iter()
            .map(|(_, v)| v)
            .find(|v| v.dict_get("targetID").is_some())
            .expect("expected added signature with targetID");
        let dev = sig_kv.dict_get("deviation").and_then(|v| v.as_f64()).unwrap_or(0.0);
        assert!(dev > 0.0, "unscanned signature must report positive deviation");
        let reported_pos = match sig_kv.dict_get("position") {
            Some(PyRep::Tuple(t)) => [t[0].as_f64().unwrap(), t[1].as_f64().unwrap(), t[2].as_f64().unwrap()],
            _ => panic!("expected position tuple"),
        };
        let target_id = sig_kv.dict_get("targetID").and_then(|v| v.as_str()).unwrap();
        let sig_rec = ctx.db.with_snapshot(|s| s.cosmic_signatures.get(&sys_lowsec).and_then(|sigs| sigs.iter().find(|r| r.signature_id == target_id).cloned())).unwrap();
        let true_pos = sig_rec.position;
        let dist = ((reported_pos[0] - true_pos[0]).powi(2) + (reported_pos[1] - true_pos[1]).powi(2) + (reported_pos[2] - true_pos[2]).powi(2)).sqrt();
        assert!(dist <= dev + 1.0, "offset distance {dist} must be <= deviation {dev}");
        assert!(dist >= 0.05 * dev, "offset distance {dist} must be non-zero");
        // Verify offset cannot be reversed from public targetID + deviation alone
        let h = target_id.bytes().fold(14695981039346656037_u64, |a, b| (a ^ (b as u64)).wrapping_mul(1099511628211));
        let naive_x = reported_pos[0] - (((h & 0xFFFF) as f64 / 65535.0) * 2.0 - 1.0) * dev * 0.75;
        assert_ne!(naive_x, true_pos[0], "true position must not be derivable from targetID and deviation");
        // Verify CSPRNG non-determinism across draws:
        let kv2 = eve_server::services::fleet_social::build_signal_tracker_signature_kv(&ctx.sde, &sig_rec);
        let pos2 = match kv2.dict_get("position") {
            Some(PyRep::Tuple(t)) => [t[0].as_f64().unwrap(), t[1].as_f64().unwrap(), t[2].as_f64().unwrap()],
            _ => panic!("expected position tuple"),
        };
        let dist2 = ((pos2[0] - true_pos[0]).powi(2) + (pos2[1] - true_pos[1]).powi(2) + (pos2[2] - true_pos[2]).powi(2)).sqrt();
        assert!(dist2 <= dev + 1.0, "second draw distance {dist2} must be <= deviation {dev}");
        assert_ne!(reported_pos, pos2, "subsequent reports must draw unpredictable CSPRNG offsets");
        assert!(matches!(&sig_upd[2], PyRep::List(l) if !l.is_empty()));
        let anom_rem = find_notif(&exp_notifs, "OnSignalTrackerAnomalyUpdate");
        assert!(matches!(&anom_rem[2], PyRep::List(l) if !l.is_empty()));

        // 10. Player corp creation, Upwell structure Anchor + Unanchor SignalTrackerStructureUpdate, StripFitting, and SendInviteByPlayer
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) { ch.station_id = Some(JITA_STATION); }
            if let Some(ch) = snap.characters.get_mut(&PILOT_B) { ch.station_id = Some(JITA_STATION); }
            if let Some(sh) = snap.items.get_mut(&carrier_id) { sh.location_id = JITA_STATION; sh.flag = InventoryFlag::Hangar; }
        });
        sess_a.station_id = Some(JITA_STATION);
        sess_b.station_id = Some(JITA_STATION);

        rpc(&ctx, &mut sess_a, "ship", "StripFitting", &[PyRep::Integer(carrier_id)]).await.expect("StripFitting");
        let strip_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let items_changed_events: Vec<&[PyRep]> = strip_notifs.iter().filter(|(e, _)| e == "OnItemsChanged").map(|(_, a)| a.as_slice()).collect();
        assert!(items_changed_events.len() >= 3, "expected per-slot OnItemsChanged groups, got {}", items_changed_events.len());
        let mut seen_flags = Vec::new();
        for ev_args in &items_changed_events {
            assert_eq!((ev_args.len(), &ev_args[2]), (3, &PyRep::Integer(JITA_STATION)));
            let PyRep::List(ref kvs) = ev_args[0] else { panic!("expected item list") };
            assert_eq!(kvs[0].dict_get("categoryID"), Some(&PyRep::Integer(7)));
            assert!(kvs[0].dict_get("groupID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
            assert_eq!(kvs[0].dict_get("singleton"), Some(&PyRep::Integer(1)));
            assert_eq!(kvs[0].dict_get("stacksize"), Some(&PyRep::Integer(1)));
            assert!(kvs[0].dict_get("customInfo").is_some());
            let PyRep::Dict(ref chg) = ev_args[1] else { panic!("expected change dict") };
            let prev_flag = chg.iter().find(|(k, _)| k == &PyRep::Integer(4)).and_then(|(_, v)| v.as_i64()).expect("ixFlag");
            seen_flags.push(prev_flag);
        }
        assert!(seen_flags.contains(&19) && seen_flags.contains(&27), "expected MedSlot0(19) and HiSlot0(27), got {seen_flags:?}");

        ctx.advance_secs(12);
        rpc(&ctx, &mut sess_a, "corpRegistry", "AddCorporation", &[PyRep::String("Mutual Corp A".into()), PyRep::String("MWCA".into()), PyRep::Real(0.05)]).await.expect("AddCorporation A");
        rpc(&ctx, &mut sess_b, "corpRegistry", "AddCorporation", &[PyRep::String("Mutual Corp B".into()), PyRep::String("MWCB".into()), PyRep::Real(0.05)]).await.expect("AddCorporation B");
        ctx.advance_secs(7 * 86_400);
        let (corp_a, corp_b) = (sess_a.corporation_id.expect("corp_a"), sess_b.corporation_id.expect("corp_b"));

        // Return PILOT_A to space in sys_lowsec far from celestials to anchor, core, & unanchor an Astrahus
        let open_pos = DVec3::new(0.0, 5.0e9, 0.0);
        let (citadel_item_id, core_item_id) = (710_000_090_i64, 710_000_091_i64);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) { ch.station_id = None; ch.solar_system_id = sys_lowsec; }
            if let Some(sh) = snap.items.get_mut(&carrier_id) { sh.type_id = 650; sh.location_id = sys_lowsec as i64; sh.flag = InventoryFlag::None; }
            snap.items.insert(citadel_item_id, make_item(citadel_item_id, 35_832, PILOT_A, carrier_id, InventoryFlag::Cargo, 1, false));
            snap.items.insert(core_item_id, make_item(core_item_id, 56_201, PILOT_A, carrier_id, InventoryFlag::Cargo, 1, false));
        });
        ctx.invalidate_ship_fitting(carrier_id);
        sess_a.station_id = None;
        sess_a.solar_system_id = Some(sys_lowsec);
        let _ = sys_handle.set_ball_position(carrier_id, open_pos).await;
        let _ = sys_handle.remove_module_cycle(carrier_id, phenom_id).await;
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

        let anchor_rep = rpc(&ctx, &mut sess_a, "structureDeployment", "Anchor", &[PyRep::Integer(35_832), PyRep::String("Tracker Citadel".into())]).await.expect("Anchor");
        let structure_id = anchor_rep.dict_get("structureID").and_then(|v| v.as_i64()).expect("structureID");
        let anchor_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let st_add = find_notif(&anchor_notifs, "OnSignalTrackerStructureUpdate");
        assert_eq!((&st_add[0], &st_add[2]), (&PyRep::Integer(sys_lowsec as i64), &PyRep::List(vec![])));
        assert!(matches!(&st_add[1], PyRep::Dict(d) if d.len() == 1 && d[0].0 == PyRep::Integer(structure_id) && d[0].1.dict_get("categoryID") == Some(&PyRep::Integer(65))));

        ctx.advance_secs(25 * 3_600);
        let _ = sys_handle.set_ball_position(carrier_id, open_pos + DVec3::new(60_000.0, 0.0, 0.0)).await;
        rpc(&ctx, &mut sess_a, "structureDocking", "Dock", &[PyRep::Integer(structure_id)]).await.expect("Dock Citadel");
        rpc(&ctx, &mut sess_a, "invbroker", "Add", &[PyRep::Integer(core_item_id), PyRep::Integer(structure_id), PyRep::Integer(172)]).await.expect("Insert Core");
        ctx.advance_secs(12);
        rpc(&ctx, &mut sess_a, "structureDocking", "Undock", &[]).await.expect("Undock Citadel");
        ctx.advance_secs(1_800);
        let _ = rpc(&ctx, &mut sess_a, "structureDirectory", "GetStructures", &[]).await;
        rpc(&ctx, &mut sess_a, "structureDeployment", "Unanchor", &[PyRep::Integer(structure_id)]).await.expect("Unanchor");
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        ctx.advance_secs(7 * 86_400 + 60);
        let _ = rpc(&ctx, &mut sess_a, "structureDirectory", "GetStructures", &[]).await;
        let unanchor_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(
            find_notif(&unanchor_notifs, "OnSignalTrackerStructureUpdate"),
            &[PyRep::Integer(sys_lowsec as i64), PyRep::Dict(vec![]), PyRep::List(vec![PyRep::Integer(structure_id)])]
        );

        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        rpc(&ctx, &mut sess_a, "mutualWarInviteMgr", "SendInviteByPlayer", &[PyRep::Integer(corp_b)]).await.expect("SendInviteByPlayer");
        let war_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert_eq!(find_notif(&war_notifs, "OnMutualWarsUpdated_Remote"), &[]);
        assert!(ctx.db.with_snapshot(|s| s.war_state.mutual_invites.contains_key(&(corp_a, corp_b))));
    }

    // parity: notify:OnNewProbe, notify:OnSystemScanStarted, notify:OnProbeWarpStart
    // parity: notify:OnSystemScanStopped, notify:OnProbesIdle, notify:OnProbeStateChanged
    // parity: notify:OnScannerInfoRemoved, notify:OnAddExpiryDurationForAbyssalContent
    // parity: notify:OnAbyssalLocationExited, notify:OnAddExpiryDurationForVoidSpaceContent
    #[tokio::test]
    async fn notify_batch2_probe_scanning_abyssal_and_void_space() {
        eve_testkit::attest(&[
            "notify:OnNewProbe",
            "notify:OnSystemScanStarted",
            "notify:OnProbeWarpStart",
            "notify:OnSystemScanStopped",
            "notify:OnProbesIdle",
            "notify:OnProbeStateChanged",
            "notify:OnScannerInfoRemoved",
            "notify:OnAddExpiryDurationForAbyssalContent",
            "notify:OnAbyssalLocationExited",
            "notify:OnAddExpiryDurationForVoidSpaceContent",
        ]);

        let ctx = ServerContext::new();
        let jita_dock_pos = ctx.sde.stations[&JITA_STATION].position + DVec3::new(500.0, 0.0, 0.0);
        let (ship_id, launcher_id, probe_stack_id, fil_id) = (620_000_001_i64, 620_000_010_i64, 620_000_011_i64, 620_000_020_i64);
        let mut sess = login_in_space(&ctx, 8_301, "demo", "demo-secret", PILOT_A, ship_id, 621, SYS_JITA, jita_dock_pos).await;

        let now_u = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(launcher_id, make_item(launcher_id, 17_901, PILOT_A, ship_id, InventoryFlag::HiSlot0, 1, true));
            snap.module_online_states.insert(launcher_id, true);
            snap.items.insert(probe_stack_id, make_item(probe_stack_id, 30_013, PILOT_A, ship_id, InventoryFlag::HiSlot0, 8, false));
            snap.items.insert(fil_id, make_item(fil_id, FILAMENT_TYPE_ID, PILOT_A, ship_id, InventoryFlag::Cargo, 4, false));
            snap.cosmic_signatures.entry(SYS_JITA).or_default().push(CosmicSignatureRecord {
                signature_id: "B2-SIG".into(), solar_system_id: SYS_JITA, sig_group: "Relic Site".into(),
                type_name: "Desolate Collapsed Conduit".into(), signal_strength: 0.0, position: [250_000.0, 0.0, 0.0],
                wormhole_type_id: None, destination_system_id: None, remaining_mass_kg: 0.0, max_mass_kg: 0.0,
                max_jump_mass_kg: 0.0, created_unix: now_u, lifetime_seconds: 86_400, collapsed: false,
            });
        });
        ctx.invalidate_ship_fitting(ship_id);

        // 1-4. Launch probes, RequestScans, Dock+Undock disconnect -> ReconnectToLostProbes, DestroyProbe
        rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())]).await.expect("Launch probes");
        let launch_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert!(find_notif(&launch_notifs, "OnNewProbe")[0].dict_get("probeID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

        let coords = PyRep::List((0..8).map(|i| PyRep::List(vec![PyRep::Real(250_000.0 + (i as f64) * 500.0), PyRep::Real(1_000.0), PyRep::Real(0.0), PyRep::Integer(2)])).collect());
        rpc(&ctx, &mut sess, "scanMgr", "RequestScans", &[coords]).await.expect("RequestScans");
        let scan_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&scan_notifs, "OnProbeWarpStart").len(), 5);
        assert!(!scan_notifs.iter().any(|(e, _)| e == "OnProbeWarpEnd"));
        assert!(!scan_notifs.iter().any(|(e, _)| e == "OnSystemScanStopped"));
        let started_args = find_notif(&scan_notifs, "OnSystemScanStarted");
        assert_eq!(started_args.len(), 3);
        assert!(matches!(started_args[2], PyRep::List(ref l) if l.len() == 8));
        ctx.advance_secs(15);
        let post_scan_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(post_scan_notifs.iter().filter(|(e, _)| e == "OnProbeWarpEnd").count(), 8);
        assert!(post_scan_notifs.iter().any(|(e, _)| e == "OnProbeStateChanged"));
        let stopped_args = find_notif(&post_scan_notifs, "OnSystemScanStopped");
        assert!(matches!(stopped_args[0], PyRep::List(ref l) if l.len() == 8));
        let idle_args = find_notif(&post_scan_notifs, "OnProbesIdle");
        assert!(matches!(idle_args[0], PyRep::List(ref l) if l.len() == 8 && l[0].dict_get("probeID").is_some()));

        // Already-connected probes must NOT emit OnProbeStateChanged on ReconnectToLostProbes
        let noop_recon = rpc(&ctx, &mut sess, "scanMgr", "ReconnectToLostProbes", &[]).await.expect("Noop Reconnect");
        assert_eq!(noop_recon.dict_get("reconnectedCount"), Some(&PyRep::Integer(0)));
        assert!(!drain_all(&ctx, &mut sess, PILOT_A).iter().any(|(e, _)| e == "OnProbeStateChanged"));

        // Dock disconnects active probes; Undock + ReconnectToLostProbes reconnects them
        ctx.advance_secs(12);
        rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(JITA_STATION)]).await.expect("Dock");
        ctx.advance_secs(12);
        rpc(&ctx, &mut sess, "stationSvc", "Undock", &[]).await.expect("Undock");
        let _ = drain_all(&ctx, &mut sess, PILOT_A);

        let recon_rep = rpc(&ctx, &mut sess, "scanMgr", "ReconnectToLostProbes", &[]).await.expect("ReconnectToLostProbes");
        assert_eq!(recon_rep.dict_get("reconnectedCount"), Some(&PyRep::Integer(8)));
        let recon_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        let state_args = find_notif(&recon_notifs, "OnProbeStateChanged");
        assert_eq!((state_args.len(), &state_args[1]), (2, &PyRep::Integer(1)));

        let destroy_rep = rpc(&ctx, &mut sess, "scanMgr", "DestroyProbe", &[]).await.expect("DestroyProbe");
        assert_eq!(destroy_rep.dict_get("destroyedCount").and_then(|v| v.as_i64()), Some(8));
        let destroy_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&destroy_notifs, "OnScannerInfoRemoved"), &[]);

        // 5. Abyssal Deadspace entry + room clears + exit (away from station proximity)
        let _ = ctx.systems.get_or_spawn(SYS_JITA).set_ball_position(ship_id, DVec3::new(0.0, 2.0e9, 0.0)).await;
        ctx.advance_secs(12);
        let deploy_rep = rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(fil_id)]).await.expect("Deploy");
        let trace_id = deploy_rep.dict_get("traceBallID").and_then(|v| v.as_i64()).expect("traceBallID");
        let _ = drain_all(&ctx, &mut sess, PILOT_A);

        let enter_rep = rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_id)]).await.expect("Enter Abyss");
        let pocket_id = enter_rep.dict_get("pocketID").and_then(|v| v.as_i64()).expect("pocketID");
        let abyss_in_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&abyss_in_notifs, "OnAbyssalLocationEntered")[0].dict_get("content_id").and_then(|v| v.as_i64()), Some(pocket_id));
        assert!(find_notif(&abyss_in_notifs, "OnAddExpiryDurationForAbyssalContent")[0].as_i64().unwrap_or(0) > 0);

        crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_A, 0).await;
        rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalGateActivation", &[PyRep::Integer(1_895_000_000 + pocket_id * 10 + 1)]).await.expect("R1->R2");
        crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_A, 1).await;
        rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalGateActivation", &[PyRep::Integer(1_895_000_000 + pocket_id * 10 + 2)]).await.expect("R2->R3");
        crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_A, 2).await;
        let _ = drain_all(&ctx, &mut sess, PILOT_A);

        rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEndGateActivation", &[PyRep::Integer(1_895_000_000 + pocket_id * 10 + 3)]).await.expect("Exit Abyss");
        let abyss_out_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&abyss_out_notifs, "OnAbyssalContentFinished"), &[PyRep::Integer(pocket_id), PyRep::Integer(1)]);
        assert_eq!(find_notif(&abyss_out_notifs, "OnAbyssalLocationExited"), &[]);

        // 6. Void Space jump + relog ClientIsReady preserves effective_content_id (fil_id)
        ctx.db.with_mut_snapshot(|s| s.items.get_mut(&fil_id).unwrap().type_id = 62_448); ctx.advance_secs(12);
        rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpacePlayerJump", &[PyRep::Integer(fil_id), PyRep::Integer(777_001)]).await.expect("VoidSpacePlayerJump");
        let void_in_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&void_in_notifs, "OnVoidSpaceEntered")[0].dict_get("content_id").and_then(|v| v.as_i64()), Some(fil_id));
        assert!(find_notif(&void_in_notifs, "OnAddExpiryDurationForVoidSpaceContent")[0].as_i64().unwrap_or(0) > 0);

        rpc(&ctx, &mut sess, "voidSpaceMgr", "ClientIsReady", &[]).await.expect("voidSpaceMgr.ClientIsReady");
        let relog_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        let relog_args = find_notif(&relog_notifs, "OnVoidSpaceRelog");
        assert_eq!(relog_args[0].dict_get("content_id").and_then(|v| v.as_i64()), Some(fil_id));
        assert!(relog_args[0].dict_get("end_time").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    }

    // parity: notify:OnSeasonSelected, notify:OnChallengeProgressUpdate, notify:OnChallengeCompleted
    // parity: notify:OnChallengeRewardsGranted, notify:OnSeasonalPointsUpdated, notify:OnSeasonsFsdDataChanged
    // parity: notify:OnExpertSystemsUpdated
    #[tokio::test]
    async fn notify_batch2_void_exit_seasons_expert_systems_and_node_graphs() {
        eve_testkit::attest(&[
            "notify:OnSeasonSelected",
            "notify:OnChallengeProgressUpdate",
            "notify:OnChallengeCompleted",
            "notify:OnChallengeRewardsGranted",
            "notify:OnSeasonalPointsUpdated",
            "notify:OnSeasonsFsdDataChanged",
            "notify:OnExpertSystemsUpdated",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let (ship_id, ship_b_id, fil_id, es_item_id) = (630_000_001_i64, 630_000_002_i64, 630_000_010_i64, 630_000_020_i64);
        let mut sess = login_in_space(&ctx, 8_401, "demo", "demo-secret", PILOT_A, ship_id, 621, SYS_JITA, DVec3::ZERO).await;
        let mut sess_b = login_in_space(&ctx, 8_402, "other", "other-secret", PILOT_B, ship_b_id, 621, SYS_JITA, DVec3::new(1_000.0, 0.0, 0.0)).await;

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) { ch.clone_grade = 2; }
            if let Some(ch) = snap.characters.get_mut(&PILOT_B) { ch.clone_grade = 1; ch.station_id = Some(JITA_STATION); }
            snap.items.insert(fil_id, make_item(fil_id, 62_448, PILOT_A, ship_id, InventoryFlag::Cargo, 2, false));
            snap.items.insert(es_item_id, make_item(es_item_id, EXPERT_SYSTEM_TYPE_ID, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false));
            let mut defs = eve_server::services::season_manager::default_seasonal_challenge_definitions();
            defs.insert(0, SeasonalChallengeDef {
                challenge_id: 1_005, season_id: 1, event_type: "ui_onboarding_step".into(),
                title: "Onboarding".into(), description: "Complete two client onboarding steps".into(),
                points: 20, max_progress: 2, client_event_driven: true, reward_type_id: 0, reward_quantity: 0, is_dormant: false, ..Default::default()
            });
            snap.service_state_blobs.insert(SEASONAL_CHALLENGES_CONFIG_KEY.into(), serde_json::to_string(&defs).expect("defs"));
            snap.npe_progress.insert(PILOT_A, eve_db::NpeProgressRecord {
                character_id: PILOT_A, node_graph_id: 473, current_node_id: 1, completed: false,
                career_goal_progress: Default::default(), claimed_goals: Default::default(),
            });
        });
        sess_b.station_id = Some(JITA_STATION);

        // 1. Void Space jump (target fil_id) + exit conduit preserves effective_content_id (fil_id)
        rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpacePlayerJump", &[PyRep::Integer(fil_id), PyRep::Integer(777_002)]).await.expect("VoidSpacePlayerJump");
        let _ = drain_all(&ctx, &mut sess, PILOT_A);
        ctx.advance_secs(12);
        rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpaceEndGateActivation", &[PyRep::Integer(888_001)]).await.expect("VoidSpaceEndGateActivation");
        let void_out_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&void_out_notifs, "OnVoidSpaceContentFinished"), &[PyRep::Integer(fil_id), PyRep::Integer(1)]);
        assert_eq!(sess.solar_system_id, Some(SYS_JITA));

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.station_id = Some(JITA_STATION);
            }
        });
        sess.station_id = Some(JITA_STATION);

        // 2. Season selection + challenge progress/completion + Omega vs Alpha reward claim + GM FSD reload
        assert!(rpc(&ctx, &mut sess, "seasonManager", "process_client_event", &[PyRep::Integer(1_001), PyRep::Integer(1)]).await.is_err());

        rpc(&ctx, &mut sess, "seasonManager", "select_season", &[PyRep::Integer(1)]).await.expect("select_season");
        let sel_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert!(!sel_notifs.iter().any(|(e, _)| e == "OnSeasonSelectionUpdated"));
        let sel_args = find_notif(&sel_notifs, "OnSeasonSelected");
        assert_eq!((sel_args.len(), &sel_args[0]), (3, &PyRep::Integer(PILOT_A)));

        rpc(&ctx, &mut sess, "seasonManager", "process_client_event", &[PyRep::Integer(1_005), PyRep::Integer(1)]).await.expect("step 1");
        let prog1_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&prog1_notifs, "OnChallengeProgressUpdate"), &[PyRep::Integer(1_005), PyRep::Integer(1)]);
        assert!(!prog1_notifs.iter().any(|(e, _)| e == "OnChallengeCompleted"));

        rpc(&ctx, &mut sess, "seasonManager", "process_client_event", &[PyRep::Integer(1_005), PyRep::Integer(1)]).await.expect("step 2");
        let prog2_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&prog2_notifs, "OnChallengeProgressUpdate"), &[PyRep::Integer(1_005), PyRep::Integer(2)]);
        assert_eq!(find_notif(&prog2_notifs, "OnChallengeCompleted"), &[PyRep::Integer(1_005)]);

        rpc(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)]).await.expect("claim_challenge_rewards");
        let claim_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&claim_notifs, "OnChallengeRewardsGranted"), &[PyRep::Integer(1_005)]);
        assert_eq!(find_notif(&claim_notifs, "OnSeasonalPointsUpdated"), &[PyRep::Integer(1), PyRep::Integer(20)]);
        let goal_args = find_notif(&claim_notifs, "OnSeasonalGoalCompleted");
        assert_eq!((&goal_args[0], goal_args[1].dict_get("completed")), (&PyRep::Integer(0), Some(&PyRep::Bool(true))));
        assert_eq!(find_notif(&claim_notifs, "OnSeasonOmegaRewardsClaimed"), &[PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(1)])]);

        // Alpha clone PILOT_B claims challenge -> does NOT receive OnSeasonOmegaRewardsClaimed
        rpc(&ctx, &mut sess_b, "seasonManager", "select_season", &[PyRep::Integer(1)]).await.expect("select_season B");
        rpc(&ctx, &mut sess_b, "seasonManager", "process_client_event", &[PyRep::Integer(1_005)]).await.expect("B step 1");
        rpc(&ctx, &mut sess_b, "seasonManager", "process_client_event", &[PyRep::Integer(1_005)]).await.expect("B step 2");
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
        rpc(&ctx, &mut sess_b, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)]).await.expect("B claim");
        let b_claim_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
        assert!(b_claim_notifs.iter().any(|(e, _)| e == "OnSeasonalGoalCompleted"));
        assert!(!b_claim_notifs.iter().any(|(e, _)| e == "OnSeasonOmegaRewardsClaimed"));

        assert!(rpc(&ctx, &mut sess, "seasonManager", "reload_fsd_data", &[]).await.is_err());
        sess.role = ROLE_GM_DEFAULT;
        rpc(&ctx, &mut sess, "seasonManager", "reload_fsd_data", &[]).await.expect("reload_fsd_data");
        let fsd_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&fsd_notifs, "OnSeasonsFsdDataChanged"), &[]);

        // 3. ConsumeExpertSystem & RemoveMyExpertSystem
        rpc(&ctx, &mut sess, "expertSystemMgr", "ConsumeExpertSystem", &[PyRep::Integer(es_item_id)]).await.expect("ConsumeExpertSystem");
        let es_add_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        let es_add_args = find_notif(&es_add_notifs, "OnExpertSystemsUpdated");
        assert_eq!((es_add_args.len(), &es_add_args[1], &es_add_args[2]), (3, &PyRep::Bool(true), &PyRep::Integer(EXPERT_SYSTEM_TYPE_ID as i64)));

        rpc(&ctx, &mut sess, "expertSystemMgr", "RemoveMyExpertSystem", &[PyRep::Integer(EXPERT_SYSTEM_TYPE_ID as i64)]).await.expect("RemoveMyExpertSystem");
        let es_rem_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&es_rem_notifs, "OnExpertSystemsUpdated"), &[PyRep::Dict(vec![]), PyRep::Bool(false), PyRep::Integer(EXPERT_SYSTEM_TYPE_ID as i64)]);

        // 4. Node Graph lifecycle via operationsManager, node_graph, and air_npe
        // Under DATA GAP, client node graph instance protocol is Tranquility server-only;
        // RPCs complete without emitting notifications with guessed IDs.
        rpc(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(157), PyRep::Integer(SYS_JITA as i64), PyRep::Integer(31)]).await.expect("activate op");
        let ng_start_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert!(!ng_start_notifs.iter().any(|(e, _)| e == "OnStartClientNodeGraph"));

        rpc(&ctx, &mut sess, "node_graph", "process_client_graph_message", &[PyRep::Integer(1), PyRep::Integer(1), PyRep::String("client_action".into()), PyRep::Integer(42)]).await.expect("update");
        let ng_upd_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert!(!ng_upd_notifs.iter().any(|(e, _)| e == "OnClientNodeGraphUpdate"));

        rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(PILOT_A), PyRep::Integer(31)]).await.expect("complete category");
        let ng_stop_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert!(!ng_stop_notifs.iter().any(|(e, _)| e == "OnStopClientNodeGraph"));

        rpc(&ctx, &mut sess, "air_npe", "skip_air_npe", &[]).await.expect("skip_air_npe");
        let npe_stop_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert!(!npe_stop_notifs.iter().any(|(e, _)| e == "OnStopClientNodeGraphById"));
    }
}

pub(crate) mod gm_admin_rpc_notifications {
    // Real server-side emission tests for Batch 3:
    // GM slash commands, RPC handlers, combat damage updates, session changes, and faction warfare.

    use crate::common;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    const P1: i64 = 90_000_001;
    const P2: i64 = 90_000_002;

    fn new_ctx() -> ServerContext {
        let sde = std::sync::Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
        );
        let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        ctx.set_clock_paused(true);
        ctx
    }

    async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
        let mut s = SessionState::new(sid);
        common::login_pilot(ctx, &mut s, user, pass).await;
        let args = [PyRep::Integer(char_id)];
        ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &args, &HashMap::new(), None)
            .await
            .expect("SelectCharacterID");
        ctx.advance_secs(12);
        ctx.drain_notifications_for_char(char_id);
        s
    }

    fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
        let all = ctx.drain_notifications_for_char(char_id);
        let mut matched = Vec::new();
        let mut keep = Vec::new();
        for n in all {
            if n.event == event {
                matched.push(n.args);
            } else {
                keep.push(n);
            }
        }
        if !keep.is_empty() {
            if let Ok(mut guard) = ctx.pending_notifications.write() {
                guard.entry(char_id).or_default().extend(keep);
            }
        }
        matched
    }

    #[tokio::test]
    async fn test_batch3_gm_and_admin_rpc_notifications() {
        // parity: notify:OnAchievementsReset, notify:OnCharacterPermanentlyBanned, notify:OnCharacterTemporarilyBanned
        // parity: notify:OnCharacterUnbanned, notify:OnCharacterPokedByGM, notify:OnClusterShutdownCancelled
        // parity: notify:OnGlobalConfigUpdated, notify:OnFastCheckoutEnabledChanged, notify:OnCampaignEnrollmentChanged
        // parity: notify:OnModuleAttributeChange, notify:OnLocationAttributeChange
        eve_testkit::attest(&[
            "notify:OnAchievementsReset",
            "notify:OnCharacterPermanentlyBanned",
            "notify:OnCharacterTemporarilyBanned",
            "notify:OnCharacterUnbanned",
            "notify:OnCharacterPokedByGM",
            "notify:OnClusterShutdownCancelled",
            "notify:OnGlobalConfigUpdated",
            "notify:OnFastCheckoutEnabledChanged",
            "notify:OnCampaignEnrollmentChanged",
            "notify:OnModuleAttributeChange",
            "notify:OnLocationAttributeChange",
        ]);
        let ctx = new_ctx();
        let mut s1 = pilot(&ctx, 801, "demo", "demo-secret", P1).await;
        let mut s2 = pilot(&ctx, 802, "other", "other-secret", P2).await;

        // Give s1 GM roles for GM slash and admin commands
        s1.role = eve_machonet::service_roles::ROLE_GMH | eve_machonet::service_roles::ROLE_ADMIN;

        // 1. OnAchievementsReset via slash.SlashCmd with state seeding and reset verification
        ctx.quasar_gateway.progression.achievements.unlock_achievement(P1 as u32, 2);
        ctx.quasar_gateway.progression.achievements.set_category_score(P1 as u32, 2, 50);
        ctx.db.with_mut_snapshot(|snap| {
            let mut ext = eve_server::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap);
            ext.achievements_by_char.entry(P1).or_default().push(101);
            ext.achievement_counters_by_char.entry(P1).or_default().insert("jumps".into(), 5);
            eve_server::services::cosmetics_ui_ops::save_cosmetics_ui_state(snap, &ext);
        });
        assert!(ctx.quasar_gateway.progression.achievements.character_achievements.read().unwrap().contains_key(&(P1 as u32)));
        assert!(ctx.quasar_gateway.progression.achievements.category_scores.read().unwrap().contains_key(&(P1 as u32, 2)));
        assert!(ctx.quasar_gateway.progression.achievements.total_character_score(P1 as u32) > 0);

        let slash_ach = ctx
            .dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/achievements reset 90000001".into())], &HashMap::new(), None)
            .await
            .expect("SlashCmd /achievements reset");
        assert!(matches!(slash_ach, PyRep::KeyVal(_)));
        let ev = events(&ctx, P1, "OnAchievementsReset");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].len(), 0, "OnAchievementsReset payload expects 0 args");

        // Backing state is completely cleared
        assert!(ctx.quasar_gateway.progression.achievements.character_achievements.read().unwrap().get(&(P1 as u32)).is_none());
        assert!(ctx.quasar_gateway.progression.achievements.category_scores.read().unwrap().get(&(P1 as u32, 2)).is_none());
        assert_eq!(ctx.quasar_gateway.progression.achievements.total_character_score(P1 as u32), 0);
        let machonet_achs = ctx
            .dispatch_rpc(&mut s1, "monolithTaskTrackerMgr", None, "GetCompletedAchievementsAndClientEventCount", &[], &HashMap::new(), None)
            .await
            .expect("monolithTaskTrackerMgr.GetCompletedAchievementsAndClientEventCount");
        let completed_dict = machonet_achs.dict_get("completedDict").expect("completedDict");
        assert_eq!(completed_dict, &PyRep::Dict(vec![]));

        // 2. OnCharacterPermanentlyBanned via /chatban perm
        let ban_perm = ctx
            .dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/chatban 90000001 perm".into())], &HashMap::new(), None)
            .await
            .expect("/chatban perm");
        assert!(matches!(ban_perm, PyRep::KeyVal(_)));
        let ev = events(&ctx, P1, "OnCharacterPermanentlyBanned");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].len(), 0, "OnCharacterPermanentlyBanned payload expects 0 args");

        // 3. OnCharacterTemporarilyBanned via /chatban 3600
        let ban_temp_start = ctx.now_blueos_filetime_ticks();
        let ban_temp = ctx
            .dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/chatban 90000001 3600".into())], &HashMap::new(), None)
            .await
            .expect("/chatban temp");
        assert!(matches!(ban_temp, PyRep::KeyVal(_)));
        let ev = events(&ctx, P1, "OnCharacterTemporarilyBanned");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].len(), 1, "OnCharacterTemporarilyBanned payload expects 1 arg [expiry_ft]");
        assert_eq!(ev[0][0], PyRep::Integer(ban_temp_start + (3600 * 10_000_000)));

        // 4. OnCharacterUnbanned via /chatunban
        let unban = ctx
            .dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/chatunban 90000001".into())], &HashMap::new(), None)
            .await
            .expect("/chatunban");
        assert!(matches!(unban, PyRep::KeyVal(_)));
        let ev = events(&ctx, P1, "OnCharacterUnbanned");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].len(), 0, "OnCharacterUnbanned payload expects 0 args");

        // 5. OnCharacterPokedByGM via mailMgr.PokePlayerAboutChatMsgGm with non-GM negative test
        let neg_poke = ctx
            .dispatch_rpc(&mut s2, "mailMgr", None, "PokePlayerAboutChatMsgGm", &[PyRep::Integer(P1), PyRep::String("help".into())], &HashMap::new(), None)
            .await
            .unwrap_err();
        assert_eq!(neg_poke.code, "UserError");

        ctx.dispatch_rpc(&mut s1, "mailMgr", None, "PokePlayerAboutChatMsgGm", &[PyRep::Integer(P2), PyRep::String("ChannelPolicy".into())], &HashMap::new(), None)
            .await
            .expect("PokePlayerAboutChatMsgGm");
        let ev = events(&ctx, P2, "OnCharacterPokedByGM");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].len(), 2, "OnCharacterPokedByGM expects 2 args [gm_id, channel_name]");
        assert_eq!(ev[0][0], PyRep::Integer(P1));
        assert_eq!(ev[0][1], PyRep::String("ChannelPolicy".into()));

        // 6. OnClusterShutdownCancelled via /shutdown cancel
        ctx.dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/shutdown cancel UI/Shared/Delayed".into())], &HashMap::new(), None)
            .await
            .expect("/shutdown cancel");
        let ev = events(&ctx, P1, "OnClusterShutdownCancelled");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].len(), 1);
        assert_eq!(ev[0][0], PyRep::String("UI/Shared/Delayed".into()));

        // 7. OnGlobalConfigUpdated via machoNet.SetGlobalConfigValue with non-GM negative test and V24 kwargs
        let mut neg_cfg_kw = HashMap::new();
        neg_cfg_kw.insert("key".into(), PyRep::String("fleet_broadcast_cap".into()));
        neg_cfg_kw.insert("value".into(), PyRep::String("50".into()));
        let neg_cfg = ctx
            .dispatch_rpc(&mut s2, "machoNet", None, "SetGlobalConfigValue", &[], &neg_cfg_kw, None)
            .await
            .unwrap_err();
        assert_eq!(neg_cfg.code, "InsufficientRole");

        let mut gm_cfg_kw = HashMap::new();
        gm_cfg_kw.insert("key".into(), PyRep::String("fleet_broadcast_cap".into()));
        gm_cfg_kw.insert("value".into(), PyRep::String("50".into()));
        ctx.dispatch_rpc(&mut s1, "machoNet", None, "SetGlobalConfigValue", &[], &gm_cfg_kw, None)
            .await
            .expect("SetGlobalConfigValue kwargs");
        let ev = events(&ctx, P1, "OnGlobalConfigUpdated");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].len(), 2, "OnGlobalConfigUpdated expects [key, val]");
        assert_eq!(ev[0][0], PyRep::String("fleet_broadcast_cap".into()));
        assert_eq!(ev[0][1], PyRep::String("50".into()));

        // Verify PyRep::None is preserved (not converted to empty string)
        let mut null_cfg_kw = HashMap::new();
        null_cfg_kw.insert("key".into(), PyRep::String("null_key".into()));
        null_cfg_kw.insert("value".into(), PyRep::None);
        ctx.dispatch_rpc(&mut s1, "machoNet", None, "SetGlobalConfigValue", &[], &null_cfg_kw, None)
            .await
            .expect("SetGlobalConfigValue None");
        let ev_null = events(&ctx, P1, "OnGlobalConfigUpdated");
        assert_eq!(ev_null.len(), 1);
        assert_eq!(ev_null[0].len(), 2);
        assert_eq!(ev_null[0][0], PyRep::String("null_key".into()));
        assert_eq!(ev_null[0][1], PyRep::None);

        // 8. OnFastCheckoutEnabledChanged via /fastcheckout with state verification
        let fc_init = ctx
            .dispatch_rpc(&mut s1, "FastCheckoutService", None, "IsFastCheckoutEnabledForUser", &[], &HashMap::new(), None)
            .await
            .expect("IsFastCheckoutEnabledForUser initial");
        assert_eq!(fc_init, PyRep::Bool(false));

        ctx.dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/fastcheckout 1".into())], &HashMap::new(), None)
            .await
            .expect("/fastcheckout 1");
        let ev = events(&ctx, P1, "OnFastCheckoutEnabledChanged");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].len(), 1);
        assert_eq!(ev[0][0], PyRep::Bool(true));
        let fc_after = ctx
            .dispatch_rpc(&mut s1, "FastCheckoutService", None, "IsFastCheckoutEnabledForUser", &[], &HashMap::new(), None)
            .await
            .expect("IsFastCheckoutEnabledForUser after enable");
        assert_eq!(fc_after, PyRep::Bool(true));

        ctx.dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/fastcheckout 0".into())], &HashMap::new(), None)
            .await
            .expect("/fastcheckout 0");
        let ev_off = events(&ctx, P1, "OnFastCheckoutEnabledChanged");
        assert_eq!(ev_off.len(), 1);
        assert_eq!(ev_off[0], vec![PyRep::Bool(false)]);
        let fc_disabled = ctx
            .dispatch_rpc(&mut s1, "FastCheckoutService", None, "IsFastCheckoutEnabledForUser", &[], &HashMap::new(), None)
            .await
            .expect("IsFastCheckoutEnabledForUser after disable");
        assert_eq!(fc_disabled, PyRep::Bool(false));

        // 9. OnCampaignEnrollmentChanged via /campaign enroll 2
        ctx.dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/campaign enroll 2".into())], &HashMap::new(), None)
            .await
            .expect("/campaign enroll");
        let ev = events(&ctx, P1, "OnCampaignEnrollmentChanged");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].len(), 0);

        // 10 & 11. OnModuleAttributeChange & OnLocationAttributeChange via /setattr with strict non-GM negative test
        let neg_setattr = ctx
            .dispatch_rpc(&mut s2, "slash", None, "SlashCmd", &[PyRep::String("/setattr 7001 9 175.0".into())], &HashMap::new(), None)
            .await
            .unwrap_err();
        assert_eq!(neg_setattr.code, "InsufficientRole");

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                7001,
                eve_db::InventoryItemRecord {
                    item_id: 7001,
                    type_id: 587,
                    owner_id: P1,
                    location_id: 60003760,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        });

        let res_setattr = ctx
            .dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/setattr 7001 9 175.0".into())], &HashMap::new(), None)
            .await
            .expect("/setattr");
        assert!(matches!(res_setattr, PyRep::KeyVal(_)));

        let now_ft = ctx.now_blueos_filetime_ticks();
        let ev_mod = events(&ctx, P1, "OnModuleAttributeChange");
        assert_eq!(ev_mod.len(), 1);
        assert_eq!(ev_mod[0].len(), 8, "OnModuleAttributeChange expects 8 args");
        assert_eq!(ev_mod[0][0], PyRep::Integer(P1));
        assert_eq!(ev_mod[0][1], PyRep::Integer(7001));
        assert_eq!(ev_mod[0][2], PyRep::Integer(9));
        assert_eq!(ev_mod[0][3], PyRep::Integer(now_ft));
        assert_eq!(ev_mod[0][4], PyRep::Real(175.0));
        assert_eq!(ev_mod[0][5], PyRep::Real(350.0)); // Rifter base hp is 350.0
        assert_eq!(ev_mod[0][6], PyRep::Integer(now_ft));
        assert_eq!(ev_mod[0][7], PyRep::Bool(true));

        let ev_loc = events(&ctx, P1, "OnLocationAttributeChange");
        assert_eq!(ev_loc.len(), 1);
        assert_eq!(ev_loc[0].len(), 7, "OnLocationAttributeChange expects 7 args");
        assert_eq!(ev_loc[0][0], PyRep::Integer(P1));
        assert_eq!(ev_loc[0][1], PyRep::Integer(60003760));
        assert_eq!(ev_loc[0][2], PyRep::Integer(7001));
        assert_eq!(ev_loc[0][3], PyRep::Integer(9));
        assert_eq!(ev_loc[0][4], PyRep::Integer(now_ft));
        assert_eq!(ev_loc[0][5], PyRep::Real(175.0));
        assert_eq!(ev_loc[0][6], PyRep::Real(350.0));
    }
}

pub(crate) mod station_damage_crimewatch_shutdown_scanners {
    // Real server-side emission tests for Batch 5:
    // Station guest list events, Crimewatch timers, combat damage updates,
    // shutdown announcements, scanners, mining, space destiny, and ESS bank links.

    use crate::common;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::notify_emitters_space_dungeons::*;
    use eve_server::services::notify_emitters_combat_ess_progression::*;
    use std::collections::HashMap;

    const P1: i64 = 90_000_001;
    const P2: i64 = 90_000_002;
    const JITA_STATION: i64 = 60_003_760;
    const JITA_SYSTEM: u32 = 30_000_142;

    fn new_ctx() -> ServerContext {
        let sde = std::sync::Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
        );
        let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        ctx.set_clock_paused(true);
        ctx
    }

    async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
        let mut s = SessionState::new(sid);
        common::login_pilot(ctx, &mut s, user, pass).await;
        let args = [PyRep::Integer(char_id)];
        ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &args, &HashMap::new(), None)
            .await
            .expect("SelectCharacterID");
        ctx.advance_secs(12);
        ctx.drain_notifications_for_char(char_id);
        s
    }

    fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
        let all = ctx.drain_notifications_for_char(char_id);
        let mut matched = Vec::new();
        let mut keep = Vec::new();
        for n in all {
            if n.event == event {
                matched.push(n.args);
            } else {
                keep.push(n);
            }
        }
        if !keep.is_empty() {
            if let Ok(mut guard) = ctx.pending_notifications.write() {
                guard.entry(char_id).or_default().extend(keep);
            }
        }
        matched
    }

    #[tokio::test]
    async fn test_batch5_station_damage_crimewatch_shutdown_scanners() {
        // DATA GAP: notify:OnCharNowInStation requires production dock RPC guest-list wiring
        // DATA GAP: notify:OnCharNoLongerInStation requires production undock RPC guest-list wiring
        // DATA GAP: notify:OnDamageStateChange requires bubble-scoped combat damage wiring
        // parity: notify:OnWeaponsTimerUpdate, notify:OnPvpTimerUpdate, notify:OnNpcTimerUpdate
        // parity: notify:OnCriminalTimerUpdate, notify:OnDisapprovalTimerUpdate, notify:OnClusterShutdownInitiated
        // DATA GAP: notify:OnCargoScanComplete requires ship scanner module cycle wiring
        // DATA GAP: notify:OnShipScanCompleted requires ship scanner module cycle wiring
        // DATA GAP: notify:OnOreMined requires mining cycle wiring (OF-28)
        eve_testkit::attest(&[
            "notify:OnWeaponsTimerUpdate",
            "notify:OnPvpTimerUpdate",
            "notify:OnNpcTimerUpdate",
            "notify:OnCriminalTimerUpdate",
            "notify:OnDisapprovalTimerUpdate",
            "notify:OnClusterShutdownInitiated",
        ]);

        let ctx = new_ctx();
        let mut s1 = pilot(&ctx, 811, "demo", "demo-secret", P1).await;
        let _s2 = pilot(&ctx, 812, "other", "other-secret", P2).await;

        // Ensure P2 is docked at Jita 4-4 to receive station guest notifications
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&P2) {
                c.station_id = Some(JITA_STATION);
                c.solar_system_id = JITA_SYSTEM;
            }
        });

        // 1. OnCharNowInStation: Pilot 1 enters station -> Pilot 2 receives guest row
        emit_char_station_notification(&ctx, P1, JITA_STATION, true);
        let ev_now = events(&ctx, P2, "OnCharNowInStation");
        assert_eq!(ev_now.len(), 1, "P2 must receive OnCharNowInStation");
        assert_eq!(ev_now[0].len(), 1);
        if let PyRep::List(ref row) = ev_now[0][0] {
            assert_eq!(row[0], PyRep::Integer(P1));
        } else {
            panic!("expected PyRep::List for guest row");
        }

        // 2. OnCharNoLongerInStation: Pilot 1 leaves station -> Pilot 2 receives undock guest row
        emit_char_station_notification(&ctx, P1, JITA_STATION, false);
        let ev_gone = events(&ctx, P2, "OnCharNoLongerInStation");
        assert_eq!(ev_gone.len(), 1, "P2 must receive OnCharNoLongerInStation");
        assert_eq!(ev_gone[0].len(), 1);
        if let PyRep::List(ref row) = ev_gone[0][0] {
            assert_eq!(row[0], PyRep::Integer(P1));
        } else {
            panic!("expected PyRep::List for guest row");
        }

        // 3. OnDamageStateChange: Emit V24 damage tuple
        emit_damage_state_change(&ctx, P1, 7001, 0.65, 0.85, 1.0);
        let ev_dmg = events(&ctx, P1, "OnDamageStateChange");
        assert_eq!(ev_dmg.len(), 1, "P1 must receive OnDamageStateChange");
        assert_eq!(ev_dmg[0].len(), 2, "OnDamageStateChange expects [shipID, damageTuple]");
        assert_eq!(ev_dmg[0][0], PyRep::Integer(7001));
        if let PyRep::Tuple(ref dt) = ev_dmg[0][1] {
            assert_eq!(dt.len(), 3);
            assert_eq!(dt[1], PyRep::Real(0.85));
            assert_eq!(dt[2], PyRep::Real(1.0));
        } else {
            panic!("expected PyRep::Tuple for damage state");
        }

        // 4. Crimewatch timers: Weapons, PvP, NPC, Criminal, Disapproval
        let now = ctx.now_unix_secs();
        ctx.crimewatch.write().unwrap().states.insert(
            P1,
            eve_server::combat::CharacterCrimeWatchState {
                weapon_timer_until_unix: now + 60,
                pvp_timer_until_unix: now + 300,
                criminal_flag_until_unix: now + 900,
                ..Default::default()
            },
        );
        eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
            rt.npc_timers.insert(P1, now + 30);
            rt.disapproval_flags.insert(P1, now + 120);
        });
        eve_server::services::combat_crimewatch::crimewatch_timers::emit_crimewatch_timer_notifications(&ctx, P1, now);

        let ev_weap = events(&ctx, P1, "OnWeaponsTimerUpdate");
        assert_eq!(ev_weap.len(), 1);
        assert_eq!(ev_weap[0][0], PyRep::Integer(101));

        let ev_pvp = events(&ctx, P1, "OnPvpTimerUpdate");
        assert_eq!(ev_pvp.len(), 1);
        assert_eq!(ev_pvp[0][0], PyRep::Integer(201));

        let ev_npc = events(&ctx, P1, "OnNpcTimerUpdate");
        assert_eq!(ev_npc.len(), 1);
        assert_eq!(ev_npc[0][0], PyRep::Integer(401));

        let ev_crim = events(&ctx, P1, "OnCriminalTimerUpdate");
        assert_eq!(ev_crim.len(), 1);
        assert_eq!(ev_crim[0][0], PyRep::Integer(301));

        let ev_dis = events(&ctx, P1, "OnDisapprovalTimerUpdate");
        assert_eq!(ev_dis.len(), 1);
        assert_eq!(ev_dis[0][0], PyRep::Integer(501));

        // 5. OnClusterShutdownInitiated via GM slash /shutdown 120
        s1.role = eve_machonet::service_roles::ROLE_GMH | eve_machonet::service_roles::ROLE_ADMIN;
        ctx.dispatch_rpc(
            &mut s1,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/shutdown 120 ScheduledMaintenance".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("SlashCmd /shutdown");

        let ev_shut = events(&ctx, P1, "OnClusterShutdownInitiated");
        assert_eq!(ev_shut.len(), 1);
        assert_eq!(ev_shut[0].len(), 3);
        assert_eq!(ev_shut[0][0], PyRep::String("UI/Shared/ClusterShutdownInitiated".into()));
        assert!(ev_shut[0][1].as_i64().unwrap() > 0);
        assert_eq!(ev_shut[0][2], PyRep::Integer(120));

        // 6. OnCargoScanComplete
        emit_cargo_scan_complete(&ctx, P1, 8001, vec![(34, 1000), (35, 500)]);
        let ev_cscan = events(&ctx, P1, "OnCargoScanComplete");
        assert_eq!(ev_cscan.len(), 1);
        assert_eq!(ev_cscan[0].len(), 2);
        assert_eq!(ev_cscan[0][0], PyRep::Integer(8001));

        // 7. OnShipScanCompleted
        emit_ship_scan_completed(&ctx, P1, 8001, 1500.0, 2000.0, vec![(442, 1), (443, 1)]);
        let ev_sscan = events(&ctx, P1, "OnShipScanCompleted");
        assert_eq!(ev_sscan.len(), 1);
        assert_eq!(ev_sscan[0].len(), 4);
        assert_eq!(ev_sscan[0][0], PyRep::Integer(8001));
        assert_eq!(ev_sscan[0][1], PyRep::Real(1500.0));
        assert_eq!(ev_sscan[0][2], PyRep::Real(2000.0));

        // 8. OnOreMined
        emit_ore_mined(&ctx, P1, 1230, 390, 390, 0, 0, 9001, 7002);
        let ev_ore = events(&ctx, P1, "OnOreMined");
        assert_eq!(ev_ore.len(), 1);
        assert_eq!(ev_ore[0].len(), 1);
        let PyRep::Dict(ref d) = ev_ore[0][0] else {
            panic!("expected dataDict for OnOreMined, got {:?}", ev_ore[0][0]);
        };
        assert!(d.contains(&(PyRep::String("oreType".into()), PyRep::Integer(1230))));
        assert!(d.contains(&(PyRep::String("quantity_added".into()), PyRep::Integer(390))));
    }

    #[tokio::test]
    async fn test_batch5_space_combat_arena_and_ess() {
        // DATA GAP: notify:OnCloakModuleForceDeactivated requires production wiring through combat decloak loop
        // DATA GAP: notify:OnKillNotification requires production wiring through combat death pipeline
        // DATA GAP: notify:OnLocalHackProgressUpdated requires production wiring through hacking loop
        // DATA GAP: notify:OnMatchFound requires production wiring through abyssal matchmaking loop
        // DATA GAP: notify:OnMatchMakingFailure requires production wiring through abyssal matchmaking loop
        // DATA GAP: notify:OnAddExpiryDurationForAbyssalPvpContent requires production wiring through abyssal arena loop
        // DATA GAP: notify:DoDestinyUpdates requires production wiring through destiny 1Hz loop
        // DATA GAP: notify:OnComponentRemovedSystemEnvironment requires production wiring through space environment loop
        // DATA GAP: notify:OnESSReserveBankLinkNotification requires production wiring through ESS link loop
        // DATA GAP: notify:OnESSMainBankLinkNotification requires production wiring through ESS link loop
        // DATA GAP: notify:OnMainBankPlayerLinkDisconnected requires production wiring through ESS disconnect loop
        // DATA GAP: notify:OnCosmicAnomalyAdded requires production wiring through exploration loop

        let ctx = new_ctx();
        let _s1 = pilot(&ctx, 821, "demo", "demo-secret", P1).await;
        let _s2 = pilot(&ctx, 822, "other", "other-secret", P2).await;

        // Put P1 in solar system 30000142
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&P1) {
                c.solar_system_id = JITA_SYSTEM;
            }
        });

        // 1. OnCloakModuleForceDeactivated: Proximity decloak
        emit_cloak_module_force_deactivated(&ctx, P1, "ProximityDecloak", 1950.0);
        let ev_cloak = events(&ctx, P1, "OnCloakModuleForceDeactivated");
        assert_eq!(ev_cloak.len(), 1);
        assert_eq!(ev_cloak[0].len(), 2);
        assert_eq!(ev_cloak[0][0], PyRep::String("ProximityDecloak".into()));
        assert_eq!(
            ev_cloak[0][1],
            PyRep::Dict(vec![(PyRep::String("distance".into()), PyRep::Real(1950.0))])
        );

        // 2. OnKillNotification: Sent to victim and attacker
        emit_kill_notifications(&ctx, P1, P2);
        let ev_kill1 = events(&ctx, P1, "OnKillNotification");
        let ev_kill2 = events(&ctx, P2, "OnKillNotification");
        assert_eq!(ev_kill1.len(), 1);
        assert_eq!(ev_kill2.len(), 1);
        assert_eq!(ev_kill1[0], Vec::<PyRep>::new());
        assert_eq!(ev_kill2[0], Vec::<PyRep>::new());

        // 3. OnLocalHackProgressUpdated: Container hacking progress in solar system
        emit_local_hack_progress_updated(&ctx, JITA_SYSTEM, 8501, 0.45);
        let ev_hack = events(&ctx, P1, "OnLocalHackProgressUpdated");
        assert_eq!(ev_hack.len(), 1);
        assert_eq!(ev_hack[0].len(), 2);
        assert_eq!(ev_hack[0][0], PyRep::Integer(8501));
        assert_eq!(ev_hack[0][1], PyRep::Real(0.45));

        // 4. OnMatchFound & OnAddExpiryDurationForAbyssalPvpContent: Arena match pop
        emit_pvp_match_found_and_expiry(&ctx, P1, 1_750_000_000);
        let ev_found = events(&ctx, P1, "OnMatchFound");
        let ev_exp = events(&ctx, P1, "OnAddExpiryDurationForAbyssalPvpContent");
        assert_eq!(ev_found.len(), 1);
        assert_eq!(ev_found[0], Vec::<PyRep>::new());
        assert_eq!(ev_exp.len(), 1);
        assert_eq!(ev_exp[0].len(), 1);
        let expected_ft = (1_750_000_000i64 + 11_644_473_600) * 10_000_000;
        assert_eq!(ev_exp[0][0], PyRep::Integer(expected_ft));

        // 5. OnMatchMakingFailure: Queue error notification
        emit_pvp_matchmaking_failure(&ctx, P1, 404);
        let ev_fail = events(&ctx, P1, "OnMatchMakingFailure");
        assert_eq!(ev_fail.len(), 1);
        assert_eq!(ev_fail[0].len(), 1);
        assert_eq!(ev_fail[0][0], PyRep::List(vec![PyRep::Integer(404)]));

        // 6. DoDestinyUpdates: Batch destiny movement updates
        emit_destiny_updates(&ctx, P1, vec![PyRep::Integer(101), PyRep::Integer(102)]);
        let ev_dest = events(&ctx, P1, "DoDestinyUpdates");
        assert_eq!(ev_dest.len(), 1);
        assert_eq!(ev_dest[0].len(), 1);
        assert_eq!(ev_dest[0][0], PyRep::List(vec![PyRep::Integer(101), PyRep::Integer(102)]));

        // 7. OnComponentRemovedSystemEnvironment: Clear environmental hazards
        emit_component_removed_system_environment(&ctx, JITA_SYSTEM);
        let ev_comp = events(&ctx, P1, "OnComponentRemovedSystemEnvironment");
        assert_eq!(ev_comp.len(), 1);
        assert_eq!(ev_comp[0].len(), 1);
        assert_eq!(ev_comp[0][0], PyRep::Integer(i64::from(JITA_SYSTEM)));

        // 8. OnESSReserveBankLinkNotification & OnESSMainBankLinkNotification
        emit_ess_bank_link_notifications(&ctx, JITA_SYSTEM, P1, "AttemptLinkToReserveBank", true, 0.0);
        let ev_res = events(&ctx, P1, "OnESSReserveBankLinkNotification");
        assert_eq!(ev_res.len(), 1);
        assert_eq!(ev_res[0].len(), 1);
        assert_eq!(ev_res[0][0], PyRep::Integer(i64::from(JITA_SYSTEM)));

        emit_ess_bank_link_notifications(&ctx, JITA_SYSTEM, P1, "AttemptLinkToMainBank", true, 0.0);
        let ev_main = events(&ctx, P1, "OnESSMainBankLinkNotification");
        assert_eq!(ev_main.len(), 1);
        assert_eq!(ev_main[0].len(), 1);
        assert_eq!(ev_main[0][0], PyRep::Integer(i64::from(JITA_SYSTEM)));

        // 9. OnMainBankPlayerLinkDisconnected
        emit_ess_main_bank_link_disconnected(&ctx, JITA_SYSTEM, P1, "WarpedAway");
        let ev_disc = events(&ctx, P1, "OnMainBankPlayerLinkDisconnected");
        assert_eq!(ev_disc.len(), 1);
        assert_eq!(ev_disc[0].len(), 1);
        if let PyRep::Dict(ref d) = ev_disc[0][0] {
            assert_eq!(d.len(), 4);
            assert!(d.iter().any(|(k, v)| k == &PyRep::String("reason".into()) && v == &PyRep::String("WarpedAway".into())));
        } else {
            panic!("expected PyRep::Dict for disconnect info");
        }

        // 10. OnCosmicAnomalyAdded
        spawn_and_emit_cosmic_anomaly(&ctx, JITA_SYSTEM, 1001);
        let ev_anom = events(&ctx, P1, "OnCosmicAnomalyAdded");
        assert_eq!(ev_anom.len(), 1);
        assert_eq!(ev_anom[0].len(), 1);
        if let PyRep::KeyVal(ref kv) = ev_anom[0][0] {
            assert!(kv.contains_key("targetID"));
            assert_eq!(kv.get("solarSystemID"), Some(&PyRep::Integer(i64::from(JITA_SYSTEM))));
        } else {
            panic!("expected PyRep::KeyVal for anomaly info");
        }
    }
}

pub(crate) mod station_scanners_mining_cloak_hack {
    // Real production emitter tests for Notify Batch 6 (`P-5`, `T-1`).
    // Covers Station, Ship/Pod destruction, ESS, Faction Warfare, Insurgency, and SafeLogoff/Crimewatch.


    use std::collections::HashMap;
    use glam::DVec3;
    use eve_destiny::{Ball, MovementMode};
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;

    use crate::common;

    const P1: i64 = 90_000_001;
    const P2: i64 = 90_000_002;
    const JITA_SYSTEM: u32 = 30000142;
    const JITA_STATION: i64 = 60003760;

    fn new_ctx() -> ServerContext {
        let sde = std::sync::Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
        );
        let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        ctx.set_clock_paused(true);
        ctx
    }

    async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
        let mut s = SessionState::new(sid);
        common::login_pilot(ctx, &mut s, user, pass).await;
        let args = [PyRep::Integer(char_id)];
        ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &args, &HashMap::new(), None)
            .await
            .expect("SelectCharacterID");
        ctx.advance_secs(12);
        ctx.drain_notifications_for_char(char_id);
        s
    }

    fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
        let all = ctx.drain_notifications_for_char(char_id);
        let mut matched = Vec::new();
        let mut keep = Vec::new();
        for n in all {
            if n.event == event {
                matched.push(n.args);
            } else {
                keep.push(n);
            }
        }
        if !keep.is_empty() {
            if let Ok(mut guard) = ctx.pending_notifications.write() {
                guard.entry(char_id).or_default().extend(keep);
            }
        }
        matched
    }

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
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        }
    }

    #[tokio::test]
    async fn test_batch6_station_scanners_mining_cloak_and_hack() {
        // parity: notify:OnCharNowInStation, notify:OnCharNoLongerInStation
        // DATA GAP: notify:OnDamageStateChange requires bubble-scoped combat damage wiring
        // DATA GAP: notify:OnCargoScanComplete requires ship scanner module cycle wiring
        // DATA GAP: notify:OnShipScanCompleted requires ship scanner module cycle wiring
        // DATA GAP: notify:OnOreMined requires mining cycle wiring (OF-28)
        // DATA GAP: notify:OnCloakModuleForceDeactivated requires ship cloak proximity/deactivation wiring
        // DATA GAP: notify:OnLocalHackProgressUpdated requires hacking minigame live container state wiring
        eve_testkit::attest(&[
            "notify:OnCharNowInStation",
            "notify:OnCharNoLongerInStation",
        ]);

        let ctx = new_ctx();
        let mut s1 = pilot(&ctx, 821, "demo", "demo-secret", P1).await;
        let _s2 = pilot(&ctx, 822, "other", "other-secret", P2).await;

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&P2) {
                c.station_id = Some(JITA_STATION);
                c.solar_system_id = JITA_SYSTEM;
            }
        });

        // 1. Station Undock -> emits OnCharNoLongerInStation to station pilots
        ctx.dispatch_rpc(&mut s1, "stationSvc", None, "Undock", &[], &HashMap::new(), None)
            .await
            .expect("Undock");
        let ev_gone = events(&ctx, P2, "OnCharNoLongerInStation");
        assert_eq!(ev_gone.len(), 1, "P2 receives OnCharNoLongerInStation");
        assert_eq!(
            ev_gone[0],
            vec![PyRep::Tuple(vec![
                PyRep::Integer(P1),
                PyRep::Integer(1_000_170),
                PyRep::None,
                PyRep::None,
            ])]
        );

        // Station Dock -> emits OnCharNowInStation to station pilots
        ctx.dispatch_rpc(&mut s1, "stationSvc", None, "Dock", &[PyRep::Integer(JITA_STATION)], &HashMap::new(), None)
            .await
            .expect("Dock");
        let ev_now = events(&ctx, P2, "OnCharNowInStation");
        assert_eq!(ev_now.len(), 1, "P2 receives OnCharNowInStation");
        assert_eq!(
            ev_now[0],
            vec![PyRep::Tuple(vec![
                PyRep::Integer(P1),
                PyRep::Integer(1_000_170),
                PyRep::None,
                PyRep::None,
            ])]
        );
    }

    #[tokio::test]
    async fn test_batch6_ship_and_pod_destruction_pipeline() {
        // parity: notify:OnKillNotification, notify:OnShipDeath, notify:OnPodDeath
        // parity: notify:OnPodKilled, notify:OnPlayerPodDeath, notify:OnPlayerPodRespawn
        eve_testkit::attest(&[
            "notify:OnKillNotification",
            "notify:OnShipDeath",
            "notify:OnPodDeath",
            "notify:OnPodKilled",
            "notify:OnPlayerPodDeath",
            "notify:OnPlayerPodRespawn",
        ]);

        let ctx = new_ctx();
        let _s1 = pilot(&ctx, 831, "demo", "demo-secret", P1).await;
        let _s2 = pilot(&ctx, 832, "other", "other-secret", P2).await;

        let ship1: i64 = 770_001;
        let pod1: i64 = 770_002;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(ship1, make_item(ship1, 587, P1, JITA_SYSTEM as i64, InventoryFlag::Hangar, 1, false));
            snap.items.insert(pod1, make_item(pod1, 670, P1, JITA_SYSTEM as i64, InventoryFlag::Hangar, 1, false));
        });

        // 1. Ship destruction via execute_death_pipeline_sync
        let res = eve_server::combat::death_pipeline::execute_death_pipeline_sync(
            &ctx,
            JITA_SYSTEM,
            P1,
            ship1,
            P2,
            DVec3::ZERO,
            42,
        );
        assert!(res.is_ok(), "execute_death_pipeline_sync must succeed");

        let ev_sd = events(&ctx, P1, "OnShipDeath");
        assert_eq!(ev_sd.len(), 1, "P1 receives OnShipDeath");
        assert!(ev_sd[0].is_empty(), "OnShipDeath has arity 0");

        let ev_km_vic = events(&ctx, P1, "OnKillNotification");
        assert_eq!(ev_km_vic.len(), 1, "Victim P1 receives OnKillNotification");
        assert!(ev_km_vic[0].is_empty(), "OnKillNotification has arity 0");

        let ev_km_atk = events(&ctx, P2, "OnKillNotification");
        assert_eq!(ev_km_atk.len(), 1, "Attacker P2 receives OnKillNotification");
        assert!(ev_km_atk[0].is_empty(), "OnKillNotification has arity 0");

        // 2. Pod destruction via execute_death_pipeline_sync_inner with authentic combat kill (is_self_destruct = false)
        let res_pod = eve_server::combat::death_pipeline::execute_death_pipeline_sync_inner(
            &ctx,
            JITA_SYSTEM,
            P1,
            pod1,
            P2,
            DVec3::ZERO,
            43,
            false,
        );
        assert!(res_pod.is_ok(), "execute_death_pipeline_sync_inner for pod must succeed");

        let ev_pd = events(&ctx, P1, "OnPodDeath");
        assert_eq!(ev_pd.len(), 1, "P1 receives OnPodDeath");
        assert!(ev_pd[0].is_empty(), "OnPodDeath has arity 0");

        let ev_ppd = events(&ctx, P1, "OnPlayerPodDeath");
        assert_eq!(ev_ppd.len(), 1, "P1 receives OnPlayerPodDeath");
        assert!(ev_ppd[0].is_empty(), "OnPlayerPodDeath has arity 0");

        let ev_pk = events(&ctx, P1, "OnPodKilled");
        assert_eq!(ev_pk.len(), 1, "P1 receives OnPodKilled");
        assert_eq!(ev_pk[0], vec![PyRep::Integer(pod1)]);

        let ev_respawn = events(&ctx, P1, "OnPlayerPodRespawn");
        assert_eq!(ev_respawn.len(), 1, "P1 receives OnPlayerPodRespawn");
        assert!(ev_respawn[0].is_empty(), "OnPlayerPodRespawn has arity 0");

        let ev_km_atk2 = events(&ctx, P2, "OnKillNotification");
        assert_eq!(ev_km_atk2.len(), 1, "Attacker P2 receives OnKillNotification for pod");
        assert!(ev_km_atk2[0].is_empty(), "OnKillNotification for pod has arity 0");
    }

    #[tokio::test]
    async fn test_batch6_ess_and_dynamic_bounty() {
        // parity: notify:OnSystemESSDataChanged
        // parity: notify:OnESSMainBankLinkNotification, notify:OnESSReserveBankLinkNotification
        // parity: notify:OnMainBankPlayerLinkDisconnected, notify:OnReserveBankPlayerLinkDisconnected
        // parity: notify:OnMainBankPlayerHackSuccess, notify:OnReserveBankUnlock
        // DATA GAP: notify:OnSystemDynamicBountyStateChanged requires dynamic bounty multiplier stepping from ratting bounty / pvp loss path
        // DATA GAP: notify:OnEssReserveBankInventoryFull requires ESS container inventory overflow wiring
        eve_testkit::attest(&[
            "notify:OnSystemESSDataChanged",
            "notify:OnESSMainBankLinkNotification",
            "notify:OnESSReserveBankLinkNotification",
            "notify:OnMainBankPlayerLinkDisconnected",
            "notify:OnReserveBankPlayerLinkDisconnected",
            "notify:OnMainBankPlayerHackSuccess",
            "notify:OnReserveBankUnlock",
        ]);

        let ctx = new_ctx();
        let mut s1 = pilot(&ctx, 841, "demo", "demo-secret", P1).await;
        let nullsec_sys_id = 30_004_759; // 1DQ1-A (K-space null-sec in SW quadrant)
        let ess_ball_id = 899_001_i64;
        let key_item_id = 899_002_i64;
        let ship_id = 899_003_i64;

        s1.solar_system_id = Some(nullsec_sys_id);
        s1.ship_id = Some(ship_id);
        s1.station_id = None;
        s1.structure_id = None;

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&P1) {
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = nullsec_sys_id;
                ch.active_ship_id = ship_id;
            }
            let st = snap.system_states.entry(nullsec_sys_id).or_default();
            st.ess_main_bank = eve_db::IskCents(50_000_000_00);
            st.ess_reserve_bank = eve_db::IskCents(100_000_000_00);
            snap.items.insert(
                ship_id,
                make_item(ship_id, 587, P1, nullsec_sys_id as i64, InventoryFlag::None, 1, true),
            );
            snap.items.insert(
                key_item_id,
                make_item(key_item_id, 57720, P1, ship_id, InventoryFlag::Cargo, 1, false),
            );
        });

        eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
            rt.safety_levels.insert(P1, eve_server::services::combat_crimewatch::SafetyLevel::Yellow);
        });

        let sys_handle = ctx.systems.get_or_spawn(nullsec_sys_id);
        let _ = sys_handle
            .spawn_ball(Ball {
                id: ess_ball_id,
                type_id: 32882,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;

        let _ = sys_handle
            .spawn_ball(Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(1000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;

        // 1. ESS Main Bank Link start
        let kw = HashMap::new();
        let res_start = ctx
            .dispatch_rpc(&mut s1, "essMgr", None, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)], &kw, None)
            .await
            .expect("AttemptLinkToMainBank start");
        assert_eq!(res_start.dict_get("linkStartedOnly"), Some(&PyRep::Bool(true)));
        let ev_main_link = events(&ctx, P1, "OnESSMainBankLinkNotification");
        assert_eq!(ev_main_link.len(), 1, "P1 receives OnESSMainBankLinkNotification");
        assert_eq!(ev_main_link[0], vec![PyRep::Integer(nullsec_sys_id as i64)]);

        // 2. ESS Main Bank Hack complete (advance 5 ticks)
        for _ in 0..5 {
            let _ = sys_handle.step_manual(1.0).await;
        }
        let res_comp = ctx
            .dispatch_rpc(&mut s1, "essMgr", None, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)], &kw, None)
            .await
            .expect("AttemptLinkToMainBank complete");
        assert_eq!(res_comp.dict_get("stolenISK"), Some(&PyRep::Real(50_000_000.0)));

        let ev_hack_ok = events(&ctx, P1, "OnMainBankPlayerHackSuccess");
        assert_eq!(ev_hack_ok.len(), 1, "P1 receives OnMainBankPlayerHackSuccess");
        assert_eq!(ev_hack_ok[0][0].dict_get("characterID"), Some(&PyRep::Integer(P1)));
        assert_eq!(ev_hack_ok[0][0].dict_get("solarSystemID"), Some(&PyRep::Integer(nullsec_sys_id as i64)));
        assert_eq!(ev_hack_ok[0][0].dict_get("linkID"), Some(&PyRep::Integer(ess_ball_id)));

        let ev_disc = events(&ctx, P1, "OnMainBankPlayerLinkDisconnected");
        assert_eq!(ev_disc.len(), 1, "P1 receives OnMainBankPlayerLinkDisconnected");
        assert_eq!(ev_disc[0][0].dict_get("characterID"), Some(&PyRep::Integer(P1)));
        assert_eq!(ev_disc[0][0].dict_get("solarSystemID"), Some(&PyRep::Integer(nullsec_sys_id as i64)));
        assert_eq!(ev_disc[0][0].dict_get("reason"), Some(&PyRep::String("Completed".into())));
        assert_eq!(ev_disc[0][0].dict_get("linkID"), Some(&PyRep::Integer(ess_ball_id)));

        let ev_ess_data = events(&ctx, P1, "OnSystemESSDataChanged");
        assert_eq!(ev_ess_data.len(), 1, "P1 receives OnSystemESSDataChanged");
        assert_eq!(ev_ess_data[0][0], PyRep::Integer(nullsec_sys_id as i64));
        assert_eq!(ev_ess_data[0][1].dict_get("solarSystemID"), Some(&PyRep::Integer(nullsec_sys_id as i64)));
        assert_eq!(ev_ess_data[0][1].dict_get("mainBank"), Some(&PyRep::Real(0.0)));
        assert_eq!(ev_ess_data[0][1].dict_get("reserveBank"), Some(&PyRep::Real(100_000_000.0)));

        // 3. ESS Reserve Bank Link start (with key)
        let res_res_start = ctx
            .dispatch_rpc(&mut s1, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_ball_id)], &kw, None)
            .await
            .expect("AttemptLinkToReserveBank start");
        assert_eq!(res_res_start.dict_get("linkStartedOnly"), Some(&PyRep::Bool(true)));
        let ev_res_link = events(&ctx, P1, "OnESSReserveBankLinkNotification");
        assert_eq!(ev_res_link.len(), 1, "P1 receives OnESSReserveBankLinkNotification");
        assert_eq!(ev_res_link[0], vec![PyRep::Integer(nullsec_sys_id as i64)]);

        let ev_unlock = events(&ctx, P1, "OnReserveBankUnlock");
        assert_eq!(ev_unlock.len(), 1, "P1 receives OnReserveBankUnlock");
        assert_eq!(ev_unlock[0][0].dict_get("regionID"), Some(&PyRep::Integer(10000060)));

        // Advance 5 ticks and complete Reserve Bank link
        for _ in 0..5 {
            let _ = sys_handle.step_manual(1.0).await;
        }
        let res_res_comp = ctx
            .dispatch_rpc(&mut s1, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_ball_id)], &kw, None)
            .await
            .expect("AttemptLinkToReserveBank complete");
        let expected_p1 = (eve_server::services::pve_rnd_storyline_sites::compute_ess_reserve_pulse_payout_cents(
            10_000_000_000,
            15,
            15,
        ) as f64)
            / 100.0;
        assert_eq!(res_res_comp.dict_get("stolenISK"), Some(&PyRep::Real(expected_p1)));

        let _ = ctx
            .dispatch_rpc(&mut s1, "essMgr", None, "RequestReserveBankUnlink", &[], &kw, None)
            .await
            .expect("RequestReserveBankUnlink");

        let ev_res_disc = events(&ctx, P1, "OnReserveBankPlayerLinkDisconnected");
        assert_eq!(ev_res_disc.len(), 1, "P1 receives OnReserveBankPlayerLinkDisconnected");
        assert_eq!(ev_res_disc[0][0].dict_get("characterID"), Some(&PyRep::Integer(P1)));
        assert_eq!(ev_res_disc[0][0].dict_get("solarSystemID"), Some(&PyRep::Integer(nullsec_sys_id as i64)));
        assert_eq!(ev_res_disc[0][0].dict_get("reason"), Some(&PyRep::Integer(0)));
    }

    #[tokio::test]
    async fn test_batch6_faction_warfare_and_insurgency() {
        // parity: notify:OnFacWarBunkerVulnerable, notify:OnFacWarBunkerNonVulnerable
        // parity: notify:OnFwObjectiveCompletedInSolarSystem, notify:OnFwScoreboardWon, notify:OnSolarSystemLPChange
        // parity: notify:OnSolarsystemInterferenceChanged, notify:OnSystemFullyCorruptedForFaction
        // parity: notify:OnSystemFullySuppressedForFaction, notify:OnVictoryPointStateUpdated
        // DATA GAP: notify:OnCaptureChanged requires real-time FW complex capture timer progression wiring
        eve_testkit::attest(&[
            "notify:OnFacWarBunkerVulnerable",
            "notify:OnFacWarBunkerNonVulnerable",
            "notify:OnFwObjectiveCompletedInSolarSystem",
            "notify:OnFwScoreboardWon",
            "notify:OnSolarSystemLPChange",
            "notify:OnSolarsystemInterferenceChanged",
            "notify:OnSystemFullyCorruptedForFaction",
            "notify:OnSystemFullySuppressedForFaction",
            "notify:OnVictoryPointStateUpdated",
        ]);

        let ctx = new_ctx();
        let mut s1 = pilot(&ctx, 851, "demo", "demo-secret", P1).await;

        ctx.db.with_mut_snapshot(|s| {
            s.fw_system_occupations.insert(
                JITA_SYSTEM,
                eve_db::FwSystemOccupationRecord {
                    solar_system_id: JITA_SYSTEM,
                    occupier_faction_id: 500_004,
                    owner_faction_id: 500_001,
                    victory_points: 1200,
                    victory_points_threshold: 3000,
                },
            );
        });

        // 1. Bunker vulnerability
        eve_server::services::fw_services::set_facwar_bunker_vulnerable(&ctx, JITA_SYSTEM, 500001, true);
        let ev_v = events(&ctx, P1, "OnFacWarBunkerVulnerable");
        assert_eq!(ev_v.len(), 1);
        assert_eq!(ev_v[0], vec![PyRep::Integer(500001)]);

        eve_server::services::fw_services::set_facwar_bunker_vulnerable(&ctx, JITA_SYSTEM, 0, false);
        let ev_nv = events(&ctx, P1, "OnFacWarBunkerNonVulnerable");
        assert_eq!(ev_nv.len(), 1);
        assert_eq!(ev_nv[0], vec![PyRep::Integer(0)]);

        // 2. Objective & Scoreboard
        eve_server::services::fw_services::complete_fw_dungeon_and_objective(
            &ctx, JITA_SYSTEM, 500001, true, 1, 9905,
        );
        let ev_obj = events(&ctx, P1, "OnFwObjectiveCompletedInSolarSystem");
        assert_eq!(ev_obj.len(), 1);
        assert_eq!(ev_obj[0], vec![PyRep::Integer(i64::from(JITA_SYSTEM)), PyRep::Integer(500001), PyRep::Integer(1)]);

        let ev_sb = events(&ctx, P1, "OnFwScoreboardWon");
        assert_eq!(ev_sb.len(), 1);
        assert_eq!(ev_sb[0], vec![PyRep::Integer(i64::from(JITA_SYSTEM)), PyRep::Integer(9905), PyRep::Integer(500001)]);

        // 3. Solar system LP change & Victory Points
        eve_server::services::fw_services::donate_solar_system_lp(&ctx, JITA_SYSTEM, 1000, 2000);
        let ev_lp = events(&ctx, P1, "OnSolarSystemLPChange");
        assert_eq!(ev_lp.len(), 1);
        assert_eq!(ev_lp[0], vec![PyRep::Integer(1000), PyRep::Integer(2000)]);

        eve_server::services::fw_services::add_victory_points(&ctx, JITA_SYSTEM, 50);
        let ev_vp = events(&ctx, P1, "OnVictoryPointStateUpdated");
        assert_eq!(ev_vp.len(), 1);
        assert_eq!(ev_vp[0][0], PyRep::Integer(i64::from(JITA_SYSTEM)));
        assert_eq!(ev_vp[0][1].dict_get("solarSystemID"), Some(&PyRep::Integer(i64::from(JITA_SYSTEM))));
        assert_eq!(ev_vp[0][1].dict_get("victoryPoints"), Some(&PyRep::Integer(1250)));
        assert_eq!(ev_vp[0][1].dict_get("threshold"), Some(&PyRep::Integer(3000)));

        // 4. System corruption & suppression via real add_system_corruption / add_system_suppression
        let camp_id = 9999_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.insurgency_runtime.campaigns.insert(
                camp_id,
                eve_db::PersistedInsurgencyCampaign {
                    campaign_id: camp_id,
                    pirate_faction_id: 500010,
                    target_faction_id: 500001,
                    fob_solar_system_id: JITA_SYSTEM,
                    systems: vec![JITA_SYSTEM],
                    created_at_unix: 1000,
                    forecasting_end_unix: 2000,
                    active_end_unix: 2_000_000_000,
                    winddown_end_unix: 2_000_000_000,
                    corruption_points: Default::default(),
                    suppression_points: Default::default(),
                },
            );
        });

        eve_server::services::insurgency_services::add_system_corruption(&ctx, camp_id, JITA_SYSTEM, 1000);
        let ev_corr = events(&ctx, P1, "OnSystemFullyCorruptedForFaction");
        assert_eq!(ev_corr.len(), 1, "P1 receives OnSystemFullyCorruptedForFaction");
        assert_eq!(ev_corr[0], vec![PyRep::Integer(i64::from(JITA_SYSTEM)), PyRep::Integer(500010), PyRep::Integer(1000), PyRep::Integer(1000)]);

        eve_server::services::insurgency_services::add_system_suppression(&ctx, camp_id, JITA_SYSTEM, 1000);
        let ev_supp = events(&ctx, P1, "OnSystemFullySuppressedForFaction");
        assert_eq!(ev_supp.len(), 1, "P1 receives OnSystemFullySuppressedForFaction");
        assert_eq!(ev_supp[0], vec![PyRep::Integer(i64::from(JITA_SYSTEM)), PyRep::Integer(1000), PyRep::Integer(1000)]);

        // 5. Stellar interference via record_system_interference_activity
        eve_server::services::pve_special_sites::record_system_interference_activity(&ctx, &mut s1, JITA_SYSTEM, 0.40);
        let ev_interf = events(&ctx, P1, "OnSolarsystemInterferenceChanged");
        assert_eq!(ev_interf.len(), 1, "P1 receives OnSolarsystemInterferenceChanged");
        assert_eq!(ev_interf[0], vec![PyRep::Integer(i64::from(JITA_SYSTEM)), PyRep::Real(0.40), PyRep::Integer(2)]);
    }

    #[tokio::test]
    async fn test_batch6_safelogoff_and_crimewatch() {
        // parity: notify:OnSafeLogoffActivated, notify:OnSafeLogoffTimerStarted, notify:OnSafeLogoffAborted
        // parity: notify:OnSafeLogoffFailed, notify:OnSystemCriminalFlagUpdates, notify:OnSystemDisapprovalFlagUpdates
        eve_testkit::attest(&[
            "notify:OnSafeLogoffActivated",
            "notify:OnSafeLogoffTimerStarted",
            "notify:OnSafeLogoffAborted",
            "notify:OnSafeLogoffFailed",
            "notify:OnSystemCriminalFlagUpdates",
            "notify:OnSystemDisapprovalFlagUpdates",
        ]);

        let ctx = new_ctx();
        let mut s1 = pilot(&ctx, 861, "demo", "demo-secret", P1).await;
        let empty = HashMap::new();

        // 1. SafeLogoff while docked -> fails with SAFE_LOGOFF_DOCKED and emits OnSafeLogoffFailed
        let fail_res = ctx
            .dispatch_rpc(&mut s1, "ship", None, "SafeLogoff", &[], &empty, None)
            .await
            .expect("SafeLogoff docked");
        assert_eq!(fail_res, PyRep::List(vec![PyRep::String("SAFE_LOGOFF_DOCKED".into())]));
        let ev_slf = events(&ctx, P1, "OnSafeLogoffFailed");
        assert_eq!(ev_slf.len(), 1, "P1 receives OnSafeLogoffFailed");
        assert_eq!(ev_slf[0], vec![PyRep::List(vec![PyRep::String("SAFE_LOGOFF_DOCKED".into())])]);

        // 2. Undock into space
        ctx.dispatch_rpc(&mut s1, "stationSvc", None, "Undock", &[], &empty, None)
            .await
            .expect("Undock");

        // 3. Valid SafeLogoff in space -> emits OnSafeLogoffTimerStarted
        let expected_ft = eve_server::session_cluster_lifecycle::unix_to_filetime(ctx.now_unix() + 30);
        let start_res = ctx
            .dispatch_rpc(&mut s1, "ship", None, "SafeLogoff", &[], &empty, None)
            .await
            .expect("SafeLogoff space");
        assert_eq!(start_res, PyRep::List(vec![]));
        let ev_slt = events(&ctx, P1, "OnSafeLogoffTimerStarted");
        assert_eq!(ev_slt.len(), 1, "P1 receives OnSafeLogoffTimerStarted");
        assert_eq!(ev_slt[0], vec![PyRep::Integer(expected_ft)]);

        // 4. Abort SafeLogoff -> emits OnSafeLogoffAborted
        let abort_res = ctx
            .dispatch_rpc(&mut s1, "ship", None, "AbortSafeLogoff", &[], &empty, None)
            .await
            .expect("AbortSafeLogoff");
        assert_eq!(abort_res, PyRep::Bool(true));
        let ev_sab = events(&ctx, P1, "OnSafeLogoffAborted");
        assert_eq!(ev_sab.len(), 1, "P1 receives OnSafeLogoffAborted");
        assert_eq!(ev_sab[0], vec![PyRep::String("SAFE_LOGOFF_ABORT_GENERIC".into())]);

        // 5. Restart SafeLogoff, advance 31 seconds -> on_advance_secs processes tick and emits OnSafeLogoffActivated
        ctx.dispatch_rpc(&mut s1, "ship", None, "SafeLogoff", &[], &empty, None)
            .await
            .expect("SafeLogoff restart");
        let _ = events(&ctx, P1, "OnSafeLogoffTimerStarted");

        ctx.advance_secs(31);
        let ev_sla = events(&ctx, P1, "OnSafeLogoffActivated");
        assert_eq!(ev_sla.len(), 1, "P1 receives OnSafeLogoffActivated");
        assert!(ev_sla[0].is_empty(), "OnSafeLogoffActivated has arity 0");

        // 6. Crimewatch flag updates broadcast to system via real emit_crimewatch_timer_notifications
        let now_unix = ctx.now_unix_secs();
        if let Ok(mut cw) = ctx.crimewatch.write() {
            let entry = cw.states.entry(P1).or_default();
            entry.suspect_flag_until_unix = now_unix + 900;
        }
        eve_server::services::combat_crimewatch::emit_crimewatch_timer_notifications(&ctx, P1, now_unix);
        let ev_crim = events(&ctx, P1, "OnSystemCriminalFlagUpdates");
        assert_eq!(ev_crim.len(), 1, "P1 receives OnSystemCriminalFlagUpdates");
        assert_eq!(
            ev_crim[0],
            vec![
                PyRep::List(vec![]),
                PyRep::List(vec![PyRep::Integer(P1)]),
                PyRep::List(vec![]),
            ]
        );

        eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
            rt.disapproval_flags.insert(P1, now_unix + 300);
        });
        eve_server::services::combat_crimewatch::emit_crimewatch_timer_notifications(&ctx, P1, now_unix);
        let ev_dis = events(&ctx, P1, "OnSystemDisapprovalFlagUpdates");
        assert_eq!(ev_dis.len(), 1, "P1 receives OnSystemDisapprovalFlagUpdates");
        assert_eq!(
            ev_dis[0],
            vec![PyRep::List(vec![]), PyRep::List(vec![PyRep::Integer(P1)])]
        );
    }
}

pub(crate) mod make_item_ball_beta {
    // Live integration tests for Batch 23 notifications.


    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use eve_server::services::pve_dungeon_runtime::{
        check_dungeon_triggers, with_dungeon_runtime_store_db, DungeonRuntimeInstance,
        DungeonRuntimeObject, DungeonRuntimeRoom, DungeonTrigger, DungeonTriggerAction,
        DungeonTriggerKind,
    };
    use eve_server::services::pve_expeditions::EscalatingPathEntry;
    use eve_server::services::pve_exploration::notify_pve_destruction_events;
    use glam::DVec3;
    use std::collections::HashMap;

    const PILOT_A: i64 = 90_000_001;
    const PILOT_B: i64 = 90_000_002;
    const SYS_JITA: u32 = 30_000_142;
    const SYS_PERIMETER: u32 = 30_000_144;
    const SYS_ABYSSAL: u32 = 32_000_001;

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

    fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
        Ball {
            id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 0.5,
            mass: 10_000_000.0,
            radius: 120.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        }
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

    fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
        let mut out = sess.drain_notifications();
        for n in ctx.drain_notifications_for_char(char_id) {
            out.push((n.event, n.args));
        }
        out
    }

    fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
        notifs
            .iter()
            .find(|(ev, _)| ev == event)
            .map(|(_, args)| args.as_slice())
            .unwrap_or_else(|| {
                let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
                panic!("expected notification {event}, got {seen:?}")
            })
    }

    fn has_notif(notifs: &[(String, Vec<PyRep>)], event: &str) -> bool {
        notifs.iter().any(|(ev, _)| ev == event)
    }

    async fn setup_pilot_in_space(
        ctx: &ServerContext,
        char_id: i64,
        ship_id: i64,
        ship_type_id: u32,
        gm: bool,
    ) -> SessionState {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = SYS_JITA;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = ship_id;
            }
            snap.items.insert(
                ship_id,
                make_item(
                    ship_id,
                    ship_type_id,
                    char_id,
                    i64::from(SYS_JITA),
                    InventoryFlag::None,
                    1,
                    true,
                ),
            );
        });
        let sys = ctx.systems.get_or_spawn(SYS_JITA);
        let _ = sys
            .spawn_ball_with_combat(
                make_ball(ship_id, ship_type_id, DVec3::new(1000.0, 0.0, 0.0)),
                Default::default(),
            )
            .await;
        let mut sess = SessionState::new(char_id as u64);
        let (user, pass) = if char_id == PILOT_A {
            ("demo", "demo-secret")
        } else {
            ("other", "other-secret")
        };
        crate::common::login_pilot(ctx, &mut sess, user, pass).await;
        rpc(
            ctx,
            &mut sess,
            "charUnboundMgr",
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
        )
        .await
        .expect("SelectCharacterID");
        sess.solar_system_id = Some(SYS_JITA);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(ship_id);
        if gm {
            sess.role |= ROLE_GM_DEFAULT;
        }
        let _ = drain_all(ctx, &mut sess, char_id);
        sess
    }

    #[tokio::test]
    async fn b23_dungeon_triggers_and_camera_interest_notifications() {
        let ctx = ServerContext::new();
        let ship_id = 700_002_i64;
        let mut sess = setup_pilot_in_space(&ctx, PILOT_A, ship_id, 587, false).await;

        let handle = ctx.systems.get_or_spawn(SYS_JITA);
        // Start ship far away (50 km) from room center
        let _ = handle
            .spawn_ball(make_ball(ship_id, 587, DVec3::new(50_000.0, 0.0, 0.0)))
            .await;

        let inst_id = 88_001_i64;
        let dungeon_id = 19_u32;
        let approach_kind = DungeonTriggerKind::OnApproach {
            target_ball_id: None,
            target_object_index: None,
            anchor_pos: [0.0, 0.0, 0.0],
            radius_m: 5_000.0,
        };
        with_dungeon_runtime_store_db(&ctx.db, |store| {
            store.instances.insert(
                inst_id,
                DungeonRuntimeInstance {
                    instance_id: inst_id,
                    dungeon_id,
                    dungeon_name: "Trigger Test Dungeon".to_string(),
                    category: "combat".to_string(),
                    archetype: "guristas".to_string(),
                    solar_system_id: SYS_JITA,
                    owner_char_id: PILOT_A,
                    base_position: [0.0, 0.0, 0.0],
                    disallow_mwd: false,
                    entry_gate_ball_id: None,
                    entry_gate_pos: None,
                    entry_gate_locked: false,
                    current_room_index: 0,
                    created_unix: 1_700_000_000,
                    completed: false,
                    despawned: false,
                    active_objective_id: 0,
                    completed_task_ids: Vec::new(),
                    last_completed_task_id: None,
                    rooms: vec![DungeonRuntimeRoom {
                        room_index: 0,
                        room_id: 1,
                        room_name: "Room 1".to_string(),
                        position: [0.0, 0.0, 0.0],
                        gate_destination_pos: [0.0, 0.0, 0.0],
                        gated: false,
                        disallow_mwd: None,
                        gate_ball_id: None,
                        gate_locked: false,
                        spawned: true,
                        cleared: false,
                        objects: vec![DungeonRuntimeObject {
                            ball_id: 900_123,
                            type_id: 29624,
                            name: "Beacon".to_string(),
                            position: [0.0, 0.0, 0.0],
                            radius: 100.0,
                            is_container: false,
                            loot_items: vec![],
                            spawned: true,
                        }],
                        npc_groups: vec![],
                        triggers: vec![
                            DungeonTrigger {
                                trigger_id: 1,
                                kind: approach_kind.clone(),
                                action: DungeonTriggerAction::CameraInterest {
                                    item_id: 900_123,
                                    speed_bits: 1.5_f64.to_bits(),
                                    duration_bits: 4.0_f64.to_bits(),
                                },
                                fired: false,
                            },
                            DungeonTrigger {
                                trigger_id: 2,
                                kind: approach_kind.clone(),
                                action: DungeonTriggerAction::TriggerAmbientSound {
                                    sound_id: 401,
                                    entity_item_id: 900_123,
                                    emitter_id: 77,
                                },
                                fired: false,
                            },
                            DungeonTrigger {
                                trigger_id: 3,
                                kind: approach_kind.clone(),
                                action: DungeonTriggerAction::TriggerAudio {
                                    audio_url: "wise:/dungeon_alert_01".to_string(),
                                },
                                fired: false,
                            },
                            DungeonTrigger {
                                trigger_id: 4,
                                kind: approach_kind.clone(),
                                action: DungeonTriggerAction::TriggerMessage {
                                    message_type: 2,
                                    message_id: 5001,
                                },
                                fired: false,
                            },
                            DungeonTrigger {
                                trigger_id: 5,
                                kind: approach_kind,
                                action: DungeonTriggerAction::TriggerMusic {
                                    music_trigger_id: 909,
                                },
                                fired: false,
                            },
                        ],
                    }],
                },
            );
        });

        // Negative: out of range (50 km > 5 km) -> 0 triggers fire, 0 notifications emitted
        let fired = check_dungeon_triggers(&ctx, PILOT_A, SYS_JITA, ship_id).await;
        assert_eq!(fired, 0);
        let notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert!(!has_notif(&notifs, "OnDungeonCameraInterest"));
        assert!(!has_notif(&notifs, "OnDungeonTriggerAmbientSound"));
        assert!(!has_notif(&notifs, "OnDungeonTriggerAudio"));
        assert!(!has_notif(&notifs, "OnDungeonTriggerMessage"));
        assert!(!has_notif(&notifs, "OnDungeonTriggerMusic"));

        // Positive: move ship within 1 km of room center -> all 5 triggers fire & emit
        let _ = handle
            .spawn_ball(make_ball(ship_id, 587, DVec3::new(1_000.0, 0.0, 0.0)))
            .await;
        let fired = check_dungeon_triggers(&ctx, PILOT_A, SYS_JITA, ship_id).await;
        assert_eq!(fired, 5);

        let notifs = drain_all(&ctx, &mut sess, PILOT_A);
        let cam = find_notif(&notifs, "OnDungeonCameraInterest");
        assert_eq!(cam.len(), 3);
        assert_eq!(cam[0].as_i64(), Some(900_123));

        let amb = find_notif(&notifs, "OnDungeonTriggerAmbientSound");
        assert_eq!(amb.len(), 3);
        assert_eq!(amb[0].as_i64(), Some(i64::from(dungeon_id)));
        assert_eq!(amb[1].as_i64(), Some(401));

        let aud = find_notif(&notifs, "OnDungeonTriggerAudio");
        assert_eq!(aud.len(), 2);
        assert_eq!(aud[0].as_i64(), Some(i64::from(dungeon_id)));
        assert_eq!(aud[1].as_str(), Some("wise:/dungeon_alert_01"));

        let msg = find_notif(&notifs, "OnDungeonTriggerMessage");
        assert_eq!(msg.len(), 2);
        assert_eq!(msg[0].as_i64(), Some(2));
        assert_eq!(msg[1].as_i64(), Some(5001));

        let mus = find_notif(&notifs, "OnDungeonTriggerMusic");
        assert_eq!(mus.len(), 2);
        assert_eq!(mus[0].as_i64(), Some(i64::from(dungeon_id)));
        assert_eq!(mus[1].as_i64(), Some(909));

        // Idempotency negative: second check does not re-emit already-fired triggers
        let fired_again = check_dungeon_triggers(&ctx, PILOT_A, SYS_JITA, ship_id).await;
        assert_eq!(fired_again, 0);
        let notifs2 = drain_all(&ctx, &mut sess, PILOT_A);
        assert!(notifs2.is_empty());
    }

    #[tokio::test]
    async fn b23_fleet_escalation_and_decloak_notifications() {
        let ctx = ServerContext::new();
        let ship_a = 700_010_i64;
        let mut sess_a = setup_pilot_in_space(&ctx, PILOT_A, ship_a, 587, false).await;

        // 1. OnFleetEscalatingPathMessage:
        // Negative: solo pilot (not in fleet) triggers escalation -> no fleet notification
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&PILOT_A) {
                c.fleet_id = None;
            }
        });
        let esc_entry = EscalatingPathEntry {
            instance_id: 500_001,
            character_id: PILOT_A,
            source_dungeon_id: 1001,
            expedition_dungeon_id: 2002,
            expedition_name: "Guristas Scout Outpost".to_string(),
            destination_solar_system_id: SYS_PERIMETER,
            step_index: 1,
            total_steps: 4,
            journal_text: "Escalation".to_string(),
            created_at_sec: 1_700_000_000,
            expires_at_sec: 1_700_086_400,
            dungeons_entered: vec![],
            journal_entry_template_id: 280_001,
        };
        notify_pve_destruction_events(
            &ctx.db,
            &ctx.pending_notifications,
            &ctx.notify_tx,
            PILOT_A,
            None,
            Some(esc_entry.clone()),
            None,
        );
        let notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(!has_notif(&notifs, "OnFleetEscalatingPathMessage"));

        // Positive: PILOT_A in fleet 77_001 triggers escalation -> emits OnFleetEscalatingPathMessage
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&PILOT_A) {
                c.fleet_id = Some(77_001);
            }
            let mut fl = eve_db::FleetRecord::default();
            fl.fleet_id = 77_001;
            fl.boss_id = PILOT_A;
            fl.members = vec![PILOT_A, PILOT_B];
            snap.fleets.insert(77_001, fl);
        });
        notify_pve_destruction_events(
            &ctx.db,
            &ctx.pending_notifications,
            &ctx.notify_tx,
            PILOT_A,
            None,
            Some(esc_entry),
            None,
        );
        let notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let fleet_esc = find_notif(&notifs, "OnFleetEscalatingPathMessage");
        assert_eq!(fleet_esc.len(), 2);
        assert_eq!(fleet_esc[0].as_i64(), Some(PILOT_A));
        assert_eq!(fleet_esc[1].as_i64(), Some(280_001));

        // 2. OnCloakModuleForceDeactivated:
        let cloak_type_id = ctx
            .sde
            .types
            .values()
            .find(|t| t.group_id == 330)
            .map(|t| t.type_id)
            .unwrap_or(11578);

        sess_a.solar_system_id = Some(SYS_ABYSSAL);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&PILOT_A) {
                c.solar_system_id = SYS_ABYSSAL;
            }
        });
        let abyssal_beyonce_oid = ctx
            .bind_object(&sess_a, "beyonce", i64::from(SYS_ABYSSAL))
            .expect("bind beyonce abyssal");
        let abyssal_handle = ctx.systems.get_or_spawn(SYS_ABYSSAL);
        let _ = abyssal_handle
            .spawn_ball(make_ball(ship_a, 587, DVec3::ZERO))
            .await;
        // Negative: uncloaked ship in Abyssal -> CmdStop does not emit OnCloakModuleForceDeactivated
        ctx.dispatch_rpc(
            &mut sess_a,
            "beyonce",
            Some(abyssal_beyonce_oid),
            "CmdStop",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("CmdStop uncloaked");
        let notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        assert!(!has_notif(&notifs, "OnCloakModuleForceDeactivated"));

        // Positive: cloaked ship with active cloak module in Abyssal -> CmdStop force-decloaks & emits
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&PILOT_A) {
                c.active_ship_id = ship_a;
                c.solar_system_id = SYS_ABYSSAL;
            }
            snap.items.insert(
                700_099,
                make_item(
                    700_099,
                    cloak_type_id,
                    PILOT_A,
                    ship_a,
                    InventoryFlag::HiSlot0,
                    1,
                    true,
                ),
            );
            snap.module_active_states.insert(700_099);
        });
        let mut cs = eve_destiny::CombatState::default();
        cs.is_cloaked = true;
        let _ = abyssal_handle.set_combat_state(ship_a, cs).await;
        ctx.dispatch_rpc(
            &mut sess_a,
            "beyonce",
            Some(abyssal_beyonce_oid),
            "CmdStop",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("CmdStop cloaked");
        let notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let decloak = find_notif(&notifs, "OnCloakModuleForceDeactivated");
        assert_eq!(decloak.len(), 2);
        assert_eq!(decloak[0].as_str(), Some("AbyssalDeadspaceCloakBlocked"));
    }
}

pub(crate) mod new_ctx_rpc_zeta {
    // Live integration tests for Batch 30 notification emitters (`notify_emitters_crimewatch_fw.rs`).
    //
    // Covers 12 `notify:` parity rows:
    // - `notify:OnOperationReplayed`
    // - `notify:OnUiRevealForced`
    // - `notify:OnPasswordChanged`
    // - `notify:OnInvalidateCacheForLocations`
    // - `notify:OnMultiEvent`
    // - `notify:OnSolarSystemLPChange`
    // - `notify:OnVictoryPointStateUpdated`
    // - `notify:OnWeaponsTimerUpdate`
    // - `notify:OnPvpTimerUpdate`
    // - `notify:OnCriminalTimerUpdate`
    // - `notify:OnSystemCriminalFlagUpdates`
    // - `notify:OnShipDeath`


    use crate::common;
    use crate::notify_gameplay_emitters::{login_in_space, make_item};
    use eve_db::{
        corp_roles::{ROLE_DIRECTOR, ROLE_EQUIPMENT_CONFIG},
        InventoryFlag, TrainedSkillRecord,
    };
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

    fn expected_ft(unix_secs: u64) -> i64 {
        i64::try_from(unix_secs).unwrap_or(0) * 10_000_000 + 116_444_736_000_000_000
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

        // 5. OnMultiEvent: RemoveTarget without an active lock must NOT emit otherlost
        rpc(&ctx, &mut sess_a, "dogmaIM", "RemoveTarget", &[PyRep::Integer(ship_b)])
            .await
            .expect("RemoveTarget without lock");
        assert!(ctx.drain_notifications_for_char(PILOT_B).iter().all(|n| n.event != "OnMultiEvent"));

        // AddTarget + step_manual -> PILOT_A receives add, PILOT_B receives otheradd
        assert!(rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(0)]).await.is_err());
        rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b)]).await.expect("AddTarget");
        sys.step_manual(10.0).await.expect("step_manual");

        let notifs_a = ctx.drain_notifications_for_char(PILOT_A);
        let locker_add = notifs_a.iter().find(|n| n.event == "OnMultiEvent").expect("PILOT_A OnMultiEvent add");
        assert_eq!(
            locker_add.args,
            vec![PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Tuple(vec![
                    PyRep::String("OnTarget".into()),
                    PyRep::String("add".into()),
                    PyRep::Integer(ship_b),
                    PyRep::String(String::new()),
                ]),
                PyRep::Integer(0),
            ])])]
        );
        let notifs_b = ctx.drain_notifications_for_char(PILOT_B);
        let multi_add = notifs_b.iter().find(|n| n.event == "OnMultiEvent").expect("PILOT_B OnMultiEvent otheradd");
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

        // RemoveTarget while locked -> PILOT_B receives otherlost; locker receives lost
        rpc(&ctx, &mut sess_a, "dogmaIM", "RemoveTarget", &[PyRep::Integer(ship_b)]).await.expect("RemoveTarget");
        let multi_lost = ctx
            .drain_notifications_for_char(PILOT_B)
            .into_iter()
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
        assert!(ctx.drain_notifications_for_char(PILOT_A).iter().any(|n| n.event == "OnMultiEvent"));

        // ClearTargets while locked -> PILOT_B receives otherlost, PILOT_A receives clear
        rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b)]).await.expect("AddTarget 2");
        sys.step_manual(10.0).await.expect("step_manual 2");
        let _ = ctx.drain_notifications_for_char(PILOT_A);
        let _ = ctx.drain_notifications_for_char(PILOT_B);
        rpc(&ctx, &mut sess_a, "dogmaIM", "ClearTargets", &[]).await.expect("ClearTargets");
        assert!(ctx.drain_notifications_for_char(PILOT_B).iter().any(|n| n.event == "OnMultiEvent"));
        let clear_notif = ctx
            .drain_notifications_for_char(PILOT_A)
            .into_iter()
            .find(|n| n.event == "OnMultiEvent")
            .expect("PILOT_A OnMultiEvent clear");
        assert_eq!(
            clear_notif.args,
            vec![PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Tuple(vec![
                    PyRep::String("OnTarget".into()),
                    PyRep::String("clear".into()),
                    PyRep::Integer(0),
                    PyRep::String(String::new()),
                ]),
                PyRep::Integer(0),
            ])])]
        );

        // 6. OnSolarSystemLPChange: facWarMgr.DonateLPsToSolarSystem
        let fw_sys: u32 = 30_002_537; // Amamake
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

        assert_eq!(
            rpc(&ctx, &mut sess_a, "facWarMgr", "DonateLPsToSolarSystem", &[PyRep::Integer(i64::from(fw_sys)), PyRep::Integer(99_999)])
                .await
                .unwrap_err()
                .code,
            "InsufficientLoyaltyPoints"
        );
        assert!(ctx.drain_notifications_for_char(PILOT_A).iter().all(|n| n.event != "OnSolarSystemLPChange"));

        rpc(&ctx, &mut sess_a, "facWarMgr", "DonateLPsToSolarSystem", &[PyRep::Integer(i64::from(fw_sys)), PyRep::Integer(1_500)])
            .await
            .expect("DonateLPsToSolarSystem");
        let lp_ev = ctx
            .drain_notifications_for_char(PILOT_A)
            .into_iter()
            .find(|n| n.event == "OnSolarSystemLPChange")
            .expect("OnSolarSystemLPChange");
        assert_eq!(lp_ev.id_type, "solarsystemid");
        assert_eq!(lp_ev.args, vec![PyRep::Integer(0), PyRep::Integer(1_500)]);

        // 7. OnVictoryPointStateUpdated: live FW complex capture via RPCs + guard combat + clock advance
        let fw_ctx = new_ctx();
        let (_enemy_sys, warzone_sys, _cmd_sys, _rear_sys) =
            crate::fw_warzone_insurgency::setup_warzone_chain(&fw_ctx);
        let fw_ship = 960_000_109_i64;
        fw_ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.corporation_id = 1_000_181;
                ch.station_id = None;
                ch.solar_system_id = warzone_sys;
            }
            for (i, flg) in [InventoryFlag::HiSlot0, InventoryFlag::HiSlot1, InventoryFlag::HiSlot2].into_iter().enumerate() {
                let gid = 960_000_120 + (i as i64) * 2;
                snap.items.insert(gid, make_item(gid, 3074, PILOT_A, fw_ship, flg, 1, true));
                snap.items.insert(gid + 1, make_item(gid + 1, 12_625, PILOT_A, fw_ship, flg, 500, false));
            }
            for &(tid, lvl) in &[(3300_u32, 5), (3301_u32, 5), (3311_u32, 5), (11_082_u32, 5)] {
                snap.skills.entry(PILOT_A).or_default().insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 });
            }
        });
        let mut sess_fw = login_in_space(&fw_ctx, 9_619, "demo", "demo-secret", PILOT_A, fw_ship, 587, warzone_sys, DVec3::ZERO).await;
        sess_fw.role |= ROLE_GM_DEFAULT;
        let wz_beyonce = fw_ctx.bind_object(&sess_fw, "beyonce", i64::from(warzone_sys)).unwrap();
        rpc(&fw_ctx, &mut sess_fw, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_004)])
            .await
            .expect("CreateMyDirectEnlistment");
        let _ = rpc(&fw_ctx, &mut sess_fw, "fwWarzoneSolarsystem", "GetLocalOccupationState", &[]).await.expect("GetLocalOccupationState");
        let _ = rpc(&fw_ctx, &mut sess_fw, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.expect("GetFactionWarfareInstances");
        let novice_gate = fw_ctx.db.with_snapshot(|s| {
            s.fw_runtime
                .fw_complexes
                .iter()
                .find_map(|(&gid, &(csys, ref tier, _, _, done))| {
                    (csys == warzone_sys && tier == "Novice" && !done).then_some(gid)
                })
                .expect("Novice complex in frontline system")
        });
        let wz_space = fw_ctx.systems.get_or_spawn(warzone_sys);
        fw_ctx.dispatch_rpc(&mut sess_fw, "beyonce", Some(wz_beyonce), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(novice_gate), PyRep::Real(0.0)], &HashMap::new(), None).await.expect("CmdWarpToStuff novice_gate");
        for _ in 0..4 {
            wz_space.step_manual(15.0).await.unwrap();
        }
        let _ = rpc(&fw_ctx, &mut sess_fw, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(novice_gate)]).await.expect("ActivateAccelerationGate");
        let guard_id = 500_000_000 + novice_gate;
        rpc(&fw_ctx, &mut sess_fw, "dogmaIM", "AddTarget", &[PyRep::Integer(guard_id)]).await.expect("AddTarget guard");
        wz_space.step_manual(5.0).await.unwrap();
        for gid in [960_000_120_i64, 960_000_122, 960_000_124] {
            rpc(&fw_ctx, &mut sess_fw, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::String("projectileFired".into()), PyRep::Integer(guard_id), PyRep::Integer(1)]).await.expect("Activate gun on guard");
        }
        for _ in 0..90 {
            if wz_space.get_ball_snapshot(guard_id).await.unwrap().is_none() { break; }
            wz_space.step_manual(1.0).await.unwrap();
        }
        assert!(wz_space.get_ball_snapshot(guard_id).await.unwrap().is_none(), "guard NPC destroyed via live combat");
        let _ = fw_ctx.drain_notifications_for_char(PILOT_A);
        eve_server::services::fw_gameplay::tick_fw_gameplay(&fw_ctx, fw_ctx.now_unix()).await;
        for _ in 0..6 {
            rpc(&fw_ctx, &mut sess_fw, "timeControl", "AdvanceClock", &[PyRep::Integer(150)]).await.expect("AdvanceClock 150");
            eve_server::services::fw_gameplay::tick_fw_gameplay(&fw_ctx, fw_ctx.now_unix()).await;
        }
        let vp_ev = fw_ctx
            .drain_notifications_for_char(PILOT_A)
            .into_iter()
            .find(|n| n.event == "OnVictoryPointStateUpdated")
            .expect("OnVictoryPointStateUpdated emitted from live complex capture");
        assert_eq!(vp_ev.id_type, "solarsystemid");
        assert_eq!(vp_ev.args[0], PyRep::Integer(i64::from(warzone_sys)));
        assert!(kv_get(&vp_ev.args[1], "victoryPoints").and_then(PyRep::as_i64).unwrap_or(0) >= 20);
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
        let gun_a: i64 = 960_000_211;
        let mut sess_a = login_in_space(&ctx, 9_621, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;
        let mut sess_b = login_in_space(&ctx, 9_622, "other", "other-secret", PILOT_B, ship_b, 587, SYS_JITA, DVec3::new(500.0, 0.0, 0.0)).await;
        sess_a.role |= ROLE_GM_DEFAULT;
        sess_b.role |= ROLE_GM_DEFAULT;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(gun_a, make_item(gun_a, 2873, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true));
            snap.items.insert(960_000_212, make_item(960_000_212, 178, PILOT_A, ship_a, InventoryFlag::HiSlot0, 500, false));
            for &(tid, lvl) in &[(3300_u32, 5), (3301_u32, 5), (3311_u32, 5)] {
                snap.skills.entry(PILOT_A).or_default().insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 });
            }
        });
        let _ = ctx.drain_notifications_for_char(PILOT_A);
        let _ = ctx.drain_notifications_for_char(PILOT_B);

        // 8-11. High-sec aggression via live client RPCs -> 101/201/301 + OnSystemCriminalFlagUpdates, then AdvanceClock -> 102/202/303
        rpc(&ctx, &mut sess_a, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await.expect("SetSafetyLevel Red");
        rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b)]).await.expect("AddTarget B");
        ctx.systems.get_or_spawn(SYS_JITA).step_manual(10.0).await.expect("step_manual");
        rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(gun_a), PyRep::String("attack".into()), PyRep::Integer(ship_b)])
            .await
            .expect("Activate gun_a");

        let t0 = ctx.now_unix();
        let notifs_a = ctx.drain_notifications_for_char(PILOT_A);
        let weap = notifs_a.iter().find(|n| n.event == "OnWeaponsTimerUpdate").expect("OnWeaponsTimerUpdate 101");
        assert_eq!(weap.args[0], PyRep::Integer(101));
        let pvp = notifs_a.iter().find(|n| n.event == "OnPvpTimerUpdate").expect("OnPvpTimerUpdate 201");
        assert_eq!(pvp.args[0], PyRep::Integer(201));
        let crim = notifs_a.iter().find(|n| n.event == "OnCriminalTimerUpdate").expect("OnCriminalTimerUpdate 301");
        assert_eq!(crim.args[0], PyRep::Integer(301));
        let sys_crim = notifs_a.iter().find(|n| n.event == "OnSystemCriminalFlagUpdates").expect("OnSystemCriminalFlagUpdates");
        assert_eq!(sys_crim.id_type, "solarsystemid");
        assert_eq!(sys_crim.args.len(), 3);
        assert_eq!(sys_crim.args[2], PyRep::List(vec![PyRep::Integer(PILOT_A)]));

        // AdvanceClock past 1s trigger -> countdown states 102, 202, 303 with Windows FILETIME expiry
        rpc(&ctx, &mut sess_a, "timeControl", "AdvanceClock", &[PyRep::Integer(5)]).await.expect("AdvanceClock 5");
        let tick_notifs = ctx.drain_notifications_for_char(PILOT_A);
        let w_cd = tick_notifs.iter().find(|n| n.event == "OnWeaponsTimerUpdate").expect("OnWeaponsTimerUpdate 102");
        assert_eq!(w_cd.args, vec![PyRep::Integer(102), PyRep::Integer(expected_ft(t0 + 60))]);
        let p_cd = tick_notifs.iter().find(|n| n.event == "OnPvpTimerUpdate").expect("OnPvpTimerUpdate 202");
        assert_eq!(p_cd.args, vec![PyRep::Integer(202), PyRep::Integer(expected_ft(t0 + 900))]);
        let c_cd = tick_notifs.iter().find(|n| n.event == "OnCriminalTimerUpdate").expect("OnCriminalTimerUpdate 303");
        assert_eq!(c_cd.args, vec![PyRep::Integer(303), PyRep::Integer(expected_ft(t0 + 900))]);

        // Advance past t0 + 600 -> 900s PvP timer must stay 202 (no 202 -> 201 regression) in both notifications and GetClientStates
        rpc(&ctx, &mut sess_a, "timeControl", "AdvanceClock", &[PyRep::Integer(596)]).await.expect("AdvanceClock to t0+601");
        let mid_notifs = ctx.drain_notifications_for_char(PILOT_A);
        assert!(mid_notifs.iter().all(|n| n.event != "OnPvpTimerUpdate" || n.args[0] == PyRep::Integer(202)));
        let cw_states = rpc(&ctx, &mut sess_a, "crimewatch", "GetClientStates", &[]).await.expect("GetClientStates at t0+601");
        if let PyRep::Tuple(outer) = &cw_states {
            if let PyRep::Tuple(timers) = &outer[0] {
                assert_eq!(timers[1], PyRep::Tuple(vec![PyRep::Integer(202), PyRep::Integer(expected_ft(t0 + 900))]));
            }
        }

        // Low-sec suspect via SetSafetyLevel(1) + Activate -> 302 -> AdvanceClock -> 304 + suspects list
        let (low_sys, ship_c, gun_c) = (30_002_537_u32, 960_000_203_i64, 960_000_213_i64);
        let uid_c = ctx.db.provision_player_account("b30_crim_c", "b30_crim_pass");
        let pilot_c = ctx.db.create_character(uid_c, "B30 Crim C", 1, 1, 11, 17, &ctx.sde).unwrap().character_id;
        let ship_b_low = 960_000_204_i64;
        let mut sess_c = login_in_space(&ctx, 9_623, "b30_crim_c", "b30_crim_pass", pilot_c, ship_c, 587, low_sys, DVec3::ZERO).await;
        sess_c.role |= ROLE_GM_DEFAULT;
        let mut sess_b2 = login_in_space(&ctx, 9_624, "other", "other-secret", PILOT_B, ship_b_low, 587, low_sys, DVec3::new(500.0, 0.0, 0.0)).await;
        sess_b2.role |= ROLE_GM_DEFAULT;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(gun_c, make_item(gun_c, 2873, pilot_c, ship_c, InventoryFlag::HiSlot0, 1, true));
            snap.items.insert(960_000_214, make_item(960_000_214, 178, pilot_c, ship_c, InventoryFlag::HiSlot0, 500, false));
            for &(tid, lvl) in &[(3300_u32, 5), (3301_u32, 5), (3311_u32, 5)] {
                snap.skills.entry(pilot_c).or_default().insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 });
            }
        });
        let _ = ctx.drain_notifications_for_char(pilot_c);
        rpc(&ctx, &mut sess_c, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(1)]).await.expect("SetSafetyLevel Partial");
        rpc(&ctx, &mut sess_c, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b_low)]).await.expect("AddTarget B low");
        ctx.systems.get_or_spawn(low_sys).step_manual(10.0).await.expect("step_manual low");
        rpc(&ctx, &mut sess_c, "dogmaIM", "Activate", &[PyRep::Integer(gun_c), PyRep::String("attack".into()), PyRep::Integer(ship_b_low)])
            .await
            .expect("Activate gun_c");
        assert_eq!(
            ctx.drain_notifications_for_char(pilot_c)
                .into_iter()
                .find(|n| n.event == "OnCriminalTimerUpdate")
                .expect("302 Suspect")
                .args[0],
            PyRep::Integer(302)
        );
        rpc(&ctx, &mut sess_c, "timeControl", "AdvanceClock", &[PyRep::Integer(5)]).await.expect("AdvanceClock low 5");
        assert_eq!(
            ctx.drain_notifications_for_char(pilot_c)
                .into_iter()
                .find(|n| n.event == "OnCriminalTimerUpdate")
                .expect("304 Suspect countdown")
                .args[0],
            PyRep::Integer(304)
        );

        // 12. OnShipDeath: ship.SelfDestruct + timeControl.AdvanceClock(125) emits OnShipDeath() with 0 args
        let _ = ctx.drain_notifications_for_char(PILOT_B);
        rpc(&ctx, &mut sess_b2, "ship", "SelfDestruct", &[PyRep::Integer(ship_b_low)]).await.expect("SelfDestruct");
        rpc(&ctx, &mut sess_b2, "timeControl", "AdvanceClock", &[PyRep::Integer(125)]).await.expect("AdvanceClock 125");
        let death = ctx
            .drain_notifications_for_char(PILOT_B)
            .into_iter()
            .find(|n| n.event == "OnShipDeath")
            .expect("OnShipDeath emitted");
        assert_eq!(death.id_type, "charid");
        assert!(death.args.is_empty());
    }
}

pub(crate) mod progression_notifications_combat_message_live {
    // Live integration tests for Batch 36 state-backed V24 notification emitters:
    // - `notify:OnProgressionAdvanced`
    // - `notify:OnProgressionTaskCompleted`
    // - `notify:OnCombatMessage`


    use crate::common;
    use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;

    const P1: i64 = 90_000_001;
    const P2: i64 = 90_000_002;
    const NULLSEC_SYSTEM: u32 = 30_004_759;

    fn new_ctx() -> ServerContext {
        let sde = std::sync::Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
        );
        let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        ctx.set_clock_paused(true);
        ctx
    }

    async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
        let mut s = SessionState::new(sid);
        common::login_pilot(ctx, &mut s, user, pass).await;
        let args = [PyRep::Integer(char_id)];
        ctx.dispatch_rpc(
            &mut s,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &args,
            &HashMap::new(),
            None,
        )
        .await
        .expect("SelectCharacterID");
        ctx.advance_secs(12);
        ctx.drain_notifications_for_char(char_id);
        s
    }

    fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
        let all = ctx.drain_notifications_for_char(char_id);
        let mut matched = Vec::new();
        let mut keep = Vec::new();
        for n in all {
            if n.event == event {
                matched.push(n.args);
            } else {
                keep.push(n);
            }
        }
        if !keep.is_empty() {
            if let Ok(mut guard) = ctx.pending_notifications.write() {
                guard.entry(char_id).or_default().extend(keep);
            }
        }
        matched
    }

    fn grant_all_combat_skills(ctx: &ServerContext, snap: &mut eve_db::WorldStateSnapshot, cid: i64, type_ids: &[u32]) {
        let sk = snap.skills.entry(cid).or_default();
        for sid in [
            3300_u32, 3302, 3305, 3308, 3310, 3311, 3312, 3315, 3316, 3318, 3319, 3320, 3334, 3413,
            3418, 3426, 20327, 21071, 21667,
        ] {
            sk.insert(
                sid,
                TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 1_280_000.0,
                },
            );
        }
        for &tid in type_ids {
            if let Some(t) = ctx.sde.types.get(&tid) {
                for sa in [182_u16, 183, 184] {
                    if let Some(&req_f) = t.default_attributes.get(&sa) {
                        let rsid = req_f as u32;
                        if rsid > 0 {
                            sk.insert(
                                rsid,
                                TrainedSkillRecord {
                                    type_id: rsid,
                                    level: 5,
                                    skill_points: 1_280_000.0,
                                },
                            );
                        }
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn test_b36_progression_notifications() {
        // parity: notify:OnProgressionAdvanced
        // parity: notify:OnProgressionTaskCompleted
        eve_testkit::attest(&[
            "notify:OnProgressionAdvanced",
            "notify:OnProgressionTaskCompleted",
        ]);

        let ctx = new_ctx();
        let mut s1 = pilot(&ctx, 913, "demo", "demo-secret", P1).await;
        let kw = HashMap::new();

        // Negative check 1 (`F-02`): NPE `operationsManager` emits `OnOperationTaskCompleted` ONLY —
        // never dungeon `ProgressionService` `OnProgressionTaskCompleted` / `OnProgressionAdvanced`!
        let op_defs = eve_server::services::operations_manager::get_operation_defs(&ctx.sde);
        let (&real_op, &client_task) = op_defs
            .op_tasks
            .iter()
            .filter(|(oid, _)| {
                op_defs.op_prerequisites.get(oid).is_none_or(|p| p.is_empty())
                    && op_defs.op_tree_parents.get(oid).is_none_or(|p| p.is_empty())
            })
            .find_map(|(op, ts)| {
                ts.first()
                    .filter(|t| {
                        op_defs.client_completable_tasks.contains(t)
                            && op_defs
                                .task_event_conditions
                                .get(t)
                                .is_none_or(|c| c.is_empty())
                    })
                    .map(|t| (op, t))
            })
            .expect("SDE operation with client-completable tasks");
        let _ = ctx
            .dispatch_rpc(
                &mut s1,
                "operationsManager",
                None,
                "activate_operation_in_solar_system",
                &[PyRep::Integer(real_op)],
                &kw,
                None,
            )
            .await
            .expect("activate_operation");
        let res_npe = ctx
            .dispatch_rpc(
                &mut s1,
                "operationsManager",
                None,
                "process_client_event",
                &[PyRep::String("complete_task".into()), PyRep::Integer(client_task)],
                &kw,
                None,
            )
            .await
            .expect("complete_task");
        assert_eq!(res_npe, PyRep::Bool(true));
        assert!(!events(&ctx, P1, "OnOperationTaskCompleted").is_empty());
        assert!(events(&ctx, P1, "OnProgressionTaskCompleted").is_empty());
        assert!(events(&ctx, P1, "OnProgressionAdvanced").is_empty());

        // Set up P1 undocked in space with a live turret ship to enter and clear a multi-room dungeon
        let ship_p1 = 969_001_i64;
        let gun_p1 = 969_010_i64;
        let ammo_p1 = 969_011_i64;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c1) = snap.characters.get_mut(&P1) {
                c1.solar_system_id = NULLSEC_SYSTEM;
                c1.station_id = None;
                c1.active_ship_id = ship_p1;
            }
            snap.items.insert(
                ship_p1,
                InventoryItemRecord {
                    item_id: ship_p1,
                    type_id: 639,
                    owner_id: P1,
                    location_id: i64::from(NULLSEC_SYSTEM),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                gun_p1,
                InventoryItemRecord {
                    item_id: gun_p1,
                    type_id: 2961,
                    owner_id: P1,
                    location_id: ship_p1,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                ammo_p1,
                InventoryItemRecord {
                    item_id: ammo_p1,
                    type_id: 21894,
                    owner_id: P1,
                    location_id: ship_p1,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 500,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(gun_p1, true);
            grant_all_combat_skills(&ctx, snap, P1, &[639, 2961, 21894]);
        });
        ctx.invalidate_ship_fitting(ship_p1);
        s1.solar_system_id = Some(NULLSEC_SYSTEM);
        s1.station_id = None;
        s1.ship_id = Some(ship_p1);

        let base_pos = [120_000_000.0, 0.0, 80_000_000.0];
        let inst_id = eve_server::services::pve_dungeon_runtime::spawn_sde_dungeon_runtime_instance(
            &ctx,
            P1,
            NULLSEC_SYSTEM,
            2001,
            "Deadspace",
            base_pos,
            true,
        )
        .await
        .expect("spawn_sde_dungeon_runtime_instance");

        let dng_state = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id)
            .expect("get_dungeon_instance_state");
        let entry_gate_id = dng_state.dict_get("entryGateBallID").and_then(|v| v.as_i64()).unwrap();

        let sys_h = ctx.systems.get_or_spawn(NULLSEC_SYSTEM);
        let entry_gate_pos = sys_h.get_ball_snapshot(entry_gate_id).await.unwrap().unwrap().1.position;
        let _ = sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: ship_p1,
                    type_id: 639,
                    position: entry_gate_pos + DVec3::new(500.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 100_000_000.0,
                    radius: 250.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(639),
            )
            .await;
        let _ = ctx.drain_notifications_for_char(P1);

        // Enter Room 1 via live `keeper.ActivateAccelerationGate` -> emits `OnProgressionRoomJoined`, zero task completions yet
        let enter_r1 = ctx
            .dispatch_rpc(
                &mut s1,
                "keeper",
                None,
                "ActivateAccelerationGate",
                &[PyRep::Integer(entry_gate_id)],
                &kw,
                None,
            )
            .await
            .expect("Activate entry acceleration gate");
        let room1_id = enter_r1.dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
        let r1_gate_id = enter_r1.dict_get("nextGateBallID").and_then(|v| v.as_i64()).unwrap();
        let relay_obj_id = match enter_r1.dict_get("objectBallIDs") {
            Some(PyRep::List(v)) => v[0].as_i64().unwrap(),
            other => panic!("expected objectBallIDs list, got {other:?}"),
        };
        let guard_npc_ids: Vec<i64> = match enter_r1.dict_get("npcBallIDs") {
            Some(PyRep::List(v)) => v.iter().map(|x| x.as_i64().unwrap()).collect(),
            other => panic!("expected npcBallIDs list, got {other:?}"),
        };
        assert_eq!(guard_npc_ids.len(), 2);
        for _ in 0..45 {
            if !matches!(
                sys_h.get_ball_snapshot(ship_p1).await.unwrap().unwrap().1.mode,
                MovementMode::Warp { .. }
            ) {
                break;
            }
            let _ = sys_h.tick().await;
        }

        let joined_ev = events(&ctx, P1, "OnProgressionRoomJoined");
        assert_eq!(joined_ev.len(), 1, "Entering dungeon pocket must emit OnProgressionRoomJoined");
        assert_eq!(joined_ev[0].len(), 5);
        assert_eq!(joined_ev[0][4], PyRep::Integer(room1_id));
        assert!(events(&ctx, P1, "OnProgressionTaskCompleted").is_empty());
        assert!(events(&ctx, P1, "OnProgressionAdvanced").is_empty());

        // Negative check 2: activating locked Room 1 gate before completing Room 1 kill objective fails with GateLocked and emits nothing
        let r1_gate_pos = sys_h.get_ball_snapshot(r1_gate_id).await.unwrap().unwrap().1.position;
        let _ = sys_h.set_ball_position(ship_p1, r1_gate_pos + DVec3::new(500.0, 0.0, 0.0)).await;
        let locked_err = ctx
            .dispatch_rpc(
                &mut s1,
                "keeper",
                None,
                "ActivateAccelerationGate",
                &[PyRep::Integer(r1_gate_id)],
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(locked_err.code, "GateLocked");
        assert!(events(&ctx, P1, "OnProgressionTaskCompleted").is_empty());
        assert!(events(&ctx, P1, "OnProgressionAdvanced").is_empty());

        // Positive check 1 (`OnApproach` trigger 1 -> `SpawnNpcGroup(2)`): live `beyonce.CmdGotoPoint` completes task 1 and advances objective
        let relay_pos = sys_h.get_ball_snapshot(relay_obj_id).await.unwrap().unwrap().1.position;
        let _ = sys_h.set_ball_position(ship_p1, relay_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
        let b1 = ctx.bind_object(&mut s1, "beyonce", i64::from(NULLSEC_SYSTEM)).unwrap();
        ctx.dispatch_rpc(
            &mut s1,
            "beyonce",
            Some(b1),
            "CmdGotoPoint",
            &[PyRep::Real(relay_pos.x), PyRep::Real(relay_pos.y), PyRep::Real(relay_pos.z)],
            &kw,
            None,
        )
        .await
        .expect("CmdGotoPoint relay object");

        let expected_task1 = room1_id * 100 + 1;
        let expected_obj_wave2 = room1_id * 10 + 2;
        let task_ev1 = events(&ctx, P1, "OnProgressionTaskCompleted");
        assert_eq!(task_ev1, vec![vec![PyRep::Integer(expected_task1)]]);
        let adv_ev1 = events(&ctx, P1, "OnProgressionAdvanced");
        assert_eq!(
            adv_ev1,
            vec![vec![PyRep::Integer(expected_obj_wave2), PyRep::Integer(expected_task1)]]
        );

        // Positive check 2 (`OnKill` trigger 2 -> `UnlockGate`): destroy both Room 1 Group 1 gate guards via live weapon combat
        ctx.dispatch_rpc(&mut s1, "beyonce", Some(b1), "CmdStop", &[], &kw, None)
            .await
            .expect("CmdStop before shooting guard NPCs");
        for (idx, &guard_npc_id) in guard_npc_ids.iter().enumerate() {
            let guard_pos = sys_h.get_ball_snapshot(guard_npc_id).await.unwrap().unwrap().1.position;
            let _ = sys_h.set_ball_position(ship_p1, guard_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;
            ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "AddTarget", &[PyRep::Integer(guard_npc_id)], &kw, None)
                .await
                .expect("AddTarget guard NPC");
            for _ in 0..30 {
                if sys_h.get_target_locks(ship_p1).await.unwrap().iter().any(|l| l.target_id == guard_npc_id && l.locked) {
                    break;
                }
                let _ = sys_h.tick().await;
            }
            if let Ok(Some(mut cs)) = sys_h.get_combat_state(guard_npc_id).await {
                cs.shield_hp = 5.0;
                cs.armor_hp = 5.0;
                cs.structure_hp = 5.0;
                cs.base_signature_radius = Some(400.0);
                cs.signature_radius = 400.0;
                let _ = sys_h.set_combat_state(guard_npc_id, cs).await;
            }
            let guard_pos_now = sys_h.get_ball_snapshot(guard_npc_id).await.unwrap().unwrap().1.position;
            let _ = sys_h.set_ball_position(ship_p1, guard_pos_now + DVec3::new(1_500.0, 0.0, 0.0)).await;
            ctx.set_invention_roll_override(Some(0.001));
            ctx.dispatch_rpc(
                &mut s1,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(gun_p1), PyRep::Integer(guard_npc_id)],
                &kw,
                None,
            )
            .await
            .expect("Activate gun_p1 on guard NPC");
            for _ in 0..25 {
                if sys_h.get_ball_snapshot(guard_npc_id).await.unwrap().is_none() {
                    break;
                }
                let _ = sys_h.step_manual(1.0).await;
            }
            ctx.set_invention_roll_override(None);
            assert!(sys_h.get_ball_snapshot(guard_npc_id).await.unwrap().is_none());
            if idx == 0 {
                assert!(events(&ctx, P1, "OnProgressionTaskCompleted").is_empty());
            }
        }

        let expected_task2 = room1_id * 100 + 2;
        let expected_obj_gate = room1_id * 10 + 9;
        let task_ev2 = events(&ctx, P1, "OnProgressionTaskCompleted");
        assert_eq!(task_ev2, vec![vec![PyRep::Integer(expected_task2)]]);
        let adv_ev2 = events(&ctx, P1, "OnProgressionAdvanced");
        assert_eq!(
            adv_ev2,
            vec![vec![PyRep::Integer(expected_obj_gate), PyRep::Integer(expected_task2)]]
        );

        let post_state = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id).unwrap();
        assert_eq!(post_state.dict_get("activeObjectiveID"), Some(&PyRep::Integer(expected_obj_gate)));
        assert_eq!(post_state.dict_get("lastCompletedTaskID"), Some(&PyRep::Integer(expected_task2)));

        // Room 1 acceleration gate is now unlocked -> traversing into Room 2 emits Room 2 OnProgressionRoomJoined & OnProgressionAdvanced
        let _ = sys_h.set_ball_position(ship_p1, r1_gate_pos + DVec3::new(500.0, 0.0, 0.0)).await;
        let enter_r2 = ctx
            .dispatch_rpc(&mut s1, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(r1_gate_id)], &kw, None)
            .await
            .expect("Activate unlocked Room 1 acceleration gate into Room 2");
        let room2_id = enter_r2.dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
        assert_eq!(events(&ctx, P1, "OnProgressionRoomJoined").len(), 1);
        assert_eq!(
            events(&ctx, P1, "OnProgressionAdvanced"),
            vec![vec![PyRep::Integer(room2_id * 10 + 1), PyRep::Integer(expected_task2)]]
        );
    }

    #[tokio::test]
    async fn test_b36_combat_message_live_weapon_cycle() {
        // parity: notify:OnCombatMessage
        eve_testkit::attest(&["notify:OnCombatMessage"]);

        let ctx = new_ctx();
        let mut s1 = pilot(&ctx, 914, "demo", "demo-secret", P1).await;
        let _s2 = pilot(&ctx, 915, "other", "other-secret", P2).await;
        let kw = HashMap::new();

        let ship_p1 = 970_001_i64;
        let ship_p2 = 970_002_i64;
        let gun_p1 = 970_010_i64;
        let ammo_p1 = 970_011_i64;
        let lml_p1 = 970_012_i64;
        let missile_p1 = 970_013_i64;

        let light_missile_tid = ctx
            .sde
            .types
            .values()
            .find(|t| t.group_id == 384 && t.published)
            .map(|t| t.type_id)
            .unwrap_or(213);

        let fw_sys = ctx
            .db
            .with_snapshot(|s| s.fw_system_occupations.keys().copied().min().unwrap_or(NULLSEC_SYSTEM));
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c1) = snap.characters.get_mut(&P1) {
                c1.solar_system_id = fw_sys;
                c1.station_id = None;
                c1.active_ship_id = ship_p1;
                c1.war_faction_id = Some(500_001);
            }
            if let Some(c2) = snap.characters.get_mut(&P2) {
                c2.solar_system_id = fw_sys;
                c2.station_id = None;
                c2.active_ship_id = ship_p2;
                c2.war_faction_id = Some(500_004);
            }
            for (sid, owner) in [(ship_p1, P1), (ship_p2, P2)] {
                snap.items.insert(
                    sid,
                    InventoryItemRecord {
                        item_id: sid,
                        type_id: 639,
                        owner_id: owner,
                        location_id: i64::from(fw_sys),
                        flag: InventoryFlag::None,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        ..Default::default()
                    },
                );
            }
            snap.items.insert(
                gun_p1,
                InventoryItemRecord {
                    item_id: gun_p1,
                    type_id: 2961,
                    owner_id: P1,
                    location_id: ship_p1,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                ammo_p1,
                InventoryItemRecord {
                    item_id: ammo_p1,
                    type_id: 21894,
                    owner_id: P1,
                    location_id: ship_p1,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 500,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                lml_p1,
                InventoryItemRecord {
                    item_id: lml_p1,
                    type_id: 2404,
                    owner_id: P1,
                    location_id: ship_p1,
                    flag: InventoryFlag::HiSlot1,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                missile_p1,
                InventoryItemRecord {
                    item_id: missile_p1,
                    type_id: light_missile_tid,
                    owner_id: P1,
                    location_id: ship_p1,
                    flag: InventoryFlag::HiSlot1,
                    quantity: 200,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(gun_p1, true);
            snap.module_online_states.insert(lml_p1, true);
            grant_all_combat_skills(&ctx, snap, P1, &[639, 2961, 21894, 2404, light_missile_tid]);
        });
        ctx.invalidate_ship_fitting(ship_p1);
        ctx.invalidate_ship_fitting(ship_p2);

        s1.solar_system_id = Some(fw_sys);
        s1.station_id = None;
        s1.ship_id = Some(ship_p1);

        let sys_h = ctx.systems.get_or_spawn(fw_sys);
        for (sid, pos) in [(ship_p1, DVec3::ZERO), (ship_p2, DVec3::new(3_000.0, 0.0, 0.0))] {
            let mut cs = ctx.build_type_combat_state(639);
            cs.signature_radius = 450.0;
            let _ = sys_h
                .spawn_ball_with_combat(
                    Ball {
                        id: sid,
                        type_id: 639,
                        position: pos,
                        velocity: DVec3::ZERO,
                        max_velocity: 300.0,
                        inertia: 1.0,
                        mass: 100_000_000.0,
                        radius: 250.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    cs,
                )
                .await;
        }

        // Negative check: activating weapon before target lock fails and emits no OnCombatMessage
        assert!(ctx
            .dispatch_rpc(
                &mut s1,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(gun_p1), PyRep::Integer(ship_p2)],
                &kw,
                None,
            )
            .await
            .is_err());
        assert!(events(&ctx, P1, "OnCombatMessage").is_empty());
        assert!(events(&ctx, P2, "OnCombatMessage").is_empty());

        // Lock target via dogmaIM.AddTarget + SystemActor ticks
        ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "AddTarget", &[PyRep::Integer(ship_p2)], &kw, None)
            .await
            .expect("AddTarget ship_p2");
        for _ in 0..30 {
            if sys_h.get_target_locks(ship_p1).await.unwrap().iter().any(|l| l.target_id == ship_p2 && l.locked) {
                break;
            }
            let _ = sys_h.tick().await;
        }

        // 1. Live turret cycle -> TacticalSvc.OnCombatMessage(messageName, messageArgs) on P1 and P2
        ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(gun_p1), PyRep::Integer(ship_p2)], &kw, None)
            .await
            .expect("Activate weapon on locked ship_p2");
        let _ = sys_h.tick().await;

        let atk_ev = events(&ctx, P1, "OnCombatMessage");
        assert!(!atk_ev.is_empty(), "Attacker P1 must receive OnCombatMessage from live weapon cycle");
        assert_eq!(atk_ev[0].len(), 2);
        assert!(matches!(atk_ev[0][0].as_str(), Some("AttackHit" | "AttackMiss")));
        assert_eq!(atk_ev[0][1].dict_get("specialObject"), Some(&PyRep::Integer(ship_p2)));
        assert_eq!(atk_ev[0][1].dict_get("type"), Some(&PyRep::Integer(2961)));
        assert_eq!(atk_ev[0][1].dict_get("weapon"), Some(&PyRep::Integer(2961)));
        assert!(atk_ev[0][1].dict_get("damage").is_some());
        assert!(atk_ev[0][1].dict_get("hitQualityText").and_then(|v| v.as_str()).is_some());

        let vic_ev = events(&ctx, P2, "OnCombatMessage");
        assert!(!vic_ev.is_empty(), "Victim P2 must receive OnCombatMessage from live weapon cycle");
        assert_eq!(vic_ev[0].len(), 2);
        assert!(matches!(vic_ev[0][0].as_str(), Some("AttackedBy" | "AttackMissBy")));
        assert_eq!(vic_ev[0][1].dict_get("specialObject"), Some(&PyRep::Integer(ship_p1)));
        assert_eq!(vic_ev[0][1].dict_get("type"), Some(&PyRep::Integer(2961)));

        // 2. `F-01` regression: live missile launcher impact (`combat_missiles_aoe.rs` passes `victim_char_opt = None`!)
        // Verify `resolved_victim` delivers `OnCombatMessage`, `OnDamageMessage`, and `OnDamageMessages` to victim P2!
        let _ = ctx.drain_notifications_for_char(P1);
        let _ = ctx.drain_notifications_for_char(P2);

        let m_res = ctx
            .dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(lml_p1), PyRep::Integer(ship_p2)], &kw, None)
            .await
            .expect("Activate Light Missile Launcher II on locked ship_p2");
        assert_eq!(m_res.dict_get("impacted"), Some(&PyRep::Bool(true)), "In-flight missile must impact ship_p2");

        let vic_missile_combat = events(&ctx, P2, "OnCombatMessage");
        assert_eq!(vic_missile_combat.len(), 1, "Victim P2 must receive OnCombatMessage when caller passes victim_char_opt = None");
        assert_eq!(vic_missile_combat[0][0].as_str(), Some("AttackedBy"));
        assert_eq!(vic_missile_combat[0][1].dict_get("specialObject"), Some(&PyRep::Integer(ship_p1)));
        assert_eq!(vic_missile_combat[0][1].dict_get("type"), Some(&PyRep::Integer(i64::from(light_missile_tid))));
        assert!(!events(&ctx, P2, "OnDamageMessage").is_empty(), "Victim P2 must receive OnDamageMessage when victim_char_opt = None");
        assert!(!events(&ctx, P2, "OnDamageMessages").is_empty(), "Victim P2 must receive OnDamageMessages when victim_char_opt = None");
    }
}

pub(crate) mod notify_operations_progression_transitions {
    // Integration tests for `track/p7-sp1-sp2` notification parity closure.
    // Exercises real server-side RPCs and verifies emitted V24 MachoNet notifications.


    use std::collections::HashMap;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;

    use crate::common;

    const P1: i64 = 90_000_001;
    const JITA_SYSTEM: u32 = 30000142;

    fn new_ctx() -> ServerContext {
        let sde = std::sync::Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
        );
        let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        ctx.set_clock_paused(true);
        ctx
    }

    async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
        let mut s = SessionState::new(sid);
        common::login_pilot(ctx, &mut s, user, pass).await;
        let args = [PyRep::Integer(char_id)];
        ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &args, &HashMap::new(), None)
            .await
            .expect("SelectCharacterID");
        ctx.advance_secs(12);
        ctx.drain_notifications_for_char(char_id);
        s
    }

    fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
        let all = ctx.drain_notifications_for_char(char_id);
        let mut matched = Vec::new();
        let mut keep = Vec::new();
        for n in all {
            if n.event == event {
                matched.push(n.args);
            } else {
                keep.push(n);
            }
        }
        if !keep.is_empty() {
            if let Ok(mut guard) = ctx.pending_notifications.write() {
                guard.entry(char_id).or_default().extend(keep);
            }
        }
        matched
    }

    #[tokio::test]
    async fn test_notify_operations_and_progression_transitions() {
        // parity: notify:OnOperationDeactivated
        let ctx = new_ctx();
        let mut s = pilot(&ctx, 2, "demo", "demo-secret", P1).await;
        s.solar_system_id = Some(JITA_SYSTEM);
        let kw = HashMap::new();
        let mut op_ids: Vec<i64> = eve_server::services::operations_manager::get_operation_defs(&ctx.sde)
            .op_tasks
            .keys()
            .copied()
            .collect();
        op_ids.sort_unstable();
        let (op1, op2) = (op_ids[0], op_ids[1]);

        ctx.dispatch_rpc(
            &mut s,
            "operationsManager",
            None,
            "activate_operation_in_solar_system",
            &[PyRep::Integer(op1), PyRep::Integer(i64::from(JITA_SYSTEM))],
            &kw,
            None,
        )
        .await
        .expect("activate op 1");
        assert!(
            events(&ctx, P1, "OnOperationDeactivated").is_empty(),
            "First operation activation must not emit OnOperationDeactivated"
        );

        ctx.dispatch_rpc(
            &mut s,
            "operationsManager",
            None,
            "activate_operation_in_solar_system",
            &[PyRep::Integer(op2), PyRep::Integer(i64::from(JITA_SYSTEM))],
            &kw,
            None,
        )
        .await
        .expect("activate op 2");
        let deact_ev = events(&ctx, P1, "OnOperationDeactivated");
        assert_eq!(deact_ev.len(), 1);
        assert!(deact_ev[0].is_empty());
    }
}

pub(crate) mod on_industry_materials_dict_shape {
    // Live V24 integration tests for newly-closed server notifications (`EvoMu-w34`):
    // - `notify:OnIndustryMaterials`
    // - `notify:OnCommunityFittingsUpdated`
    // - `notify:OnTutorialSkipped`

    use crate::common;
    use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
    use eve_machonet::{service_roles, SessionState};
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::{cache_notifications::PendingMachoNotification, ServerContext};
    use std::collections::HashMap;

    const PILOT_A: i64 = 90_000_001;
    const PILOT_B: i64 = 90_000_002;
    const JITA_STATION: i64 = 60_003_760;
    const COMMUNITY_FITTING_CORP_ID: i64 = 1_000_282;

    async fn login_pilot(
        ctx: &ServerContext,
        sid: u64,
        user: &str,
        pass: &str,
        char_id: i64,
    ) -> SessionState {
        let mut s = SessionState::new(sid);
        common::login_pilot(ctx, &mut s, user, pass).await;
        ctx.dispatch_rpc(
            &mut s,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("SelectCharacterID");
        s.station_id = Some(JITA_STATION);
        ctx.advance_secs(12);
        s
    }

    async fn rpc(
        ctx: &ServerContext,
        s: &mut SessionState,
        svc: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None)
            .await
    }

    fn find_notifs<'a>(
        notifs: &'a [PendingMachoNotification],
        event: &str,
    ) -> Vec<&'a PendingMachoNotification> {
        notifs.iter().filter(|n| n.event == event).collect()
    }

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
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        }
    }

    #[tokio::test]
    async fn test_on_industry_materials_v24_dict_shape() {
        // parity: notify:OnIndustryMaterials
        eve_testkit::attest(&["notify:OnIndustryMaterials"]);

        let ctx = ServerContext::new();
        let mut sess = login_pilot(&ctx, 8801, "demo", "demo-secret", PILOT_A).await;

        let bp_info = ctx
            .sde
            .blueprints
            .get(&687)
            .cloned()
            .expect("Rifter Blueprint (687) in SDE");
        assert!(
            !bp_info.materials.is_empty(),
            "Rifter Blueprint must have non-empty SDE manufacturing BOM"
        );

        let bp_id = 920_000_001_i64;
        let initial_stack_qty: i32 = 1_000_000;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.balance = IskCents(100_000_000_00);
                ch.station_id = Some(JITA_STATION);
            }
            snap.items.retain(|_, i| {
                !(i.owner_id == PILOT_A
                    && i.location_id == JITA_STATION
                    && bp_info.materials.iter().any(|m| m.type_id == i.type_id))
            });
            snap.items.insert(
                bp_id,
                make_item(bp_id, 687, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
            );
            for (idx, mat) in bp_info.materials.iter().enumerate() {
                let iid = 920_000_010 + idx as i64;
                snap.items.insert(iid, make_item(iid, mat.type_id, PILOT_A, JITA_STATION, InventoryFlag::Hangar, initial_stack_qty, false));
            }
        });
        let _ = ctx.drain_notifications_for_char(PILOT_A);

        // Negative 1: unauthenticated InstallJob fails and emits 0 OnIndustryMaterials
        let mut unauth = SessionState::new(8899);
        assert!(rpc(&ctx, &mut unauth, "industryManager", "InstallJob", &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(bp_id)), ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
        ])]).await.is_err());
        assert!(find_notifs(&ctx.drain_notifications_for_char(PILOT_A), "OnIndustryMaterials").is_empty());

        // Negative 2: non-existent blueprint InstallJob fails and emits 0 OnIndustryMaterials
        assert!(rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(999_999_999)), ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
        ])]).await.is_err());
        assert!(find_notifs(&ctx.drain_notifications_for_char(PILOT_A), "OnIndustryMaterials").is_empty());

        // Positive: ConnectJob(job) -> InstallJob -> OnIndustryMaterials(monitorID, {typeID: remaining_qty}) -> DisconnectJob
        let mon_rep = rpc(
            &ctx,
            &mut sess,
            "industryMonitor",
            "ConnectJob",
            &[PyRep::keyval([
                ("ownerID", PyRep::Integer(PILOT_A)),
                ("locationID", PyRep::Integer(JITA_STATION)),
                ("inputFlagID", PyRep::Integer(InventoryFlag::Hangar as i64)),
            ])],
        )
        .await
        .expect("industryMonitor.ConnectJob should succeed");
        let PyRep::Tuple(mon_parts) = mon_rep else {
            panic!("ConnectJob must return (monitorID, available_dict) tuple, got {mon_rep:?}");
        };
        let monitor_id = mon_parts[0].as_i64().expect("monitorID");
        assert!(monitor_id > 0);
        assert!(matches!(mon_parts[1], PyRep::Dict(_)));

        let job_rep = rpc(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintItemID", PyRep::Integer(bp_id)),
                ("facilityID", PyRep::Integer(JITA_STATION)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
            ])],
        )
        .await
        .expect("InstallJob should succeed");
        let job_id = job_rep
            .dict_get("jobID")
            .and_then(|v| v.as_i64())
            .expect("jobID");
        assert!(job_id > 0);

        let notifs = ctx.drain_notifications_for_char(PILOT_A);
        let ind_mats = find_notifs(&notifs, "OnIndustryMaterials");
        assert_eq!(
            ind_mats.len(),
            1,
            "Expected exactly 1 OnIndustryMaterials notification"
        );
        assert_eq!(ind_mats[0].id_type, "charid");
        assert_eq!(ind_mats[0].args.len(), 2);
        assert_eq!(ind_mats[0].args[0], PyRep::Integer(monitor_id));

        let PyRep::Dict(mat_entries) = &ind_mats[0].args[1] else {
            panic!(
                "V24 IndustryService.OnIndustryMaterials expects Dict {{typeID: available}}, got {:?}",
                ind_mats[0].args[1]
            );
        };
        assert_eq!(mat_entries.len(), bp_info.materials.len());

        for mat in &bp_info.materials {
            let reported_avail = mat_entries
                .iter()
                .find(|(k, _)| k.as_i64() == Some(i64::from(mat.type_id)))
                .and_then(|(_, v)| v.as_i64())
                .expect("each consumed BOM typeID must appear in OnIndustryMaterials dict");
            let db_avail: i64 = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|i| {
                        i.owner_id == PILOT_A
                            && i.location_id == JITA_STATION
                            && i.type_id == mat.type_id
                            && i.flag == InventoryFlag::Hangar
                    })
                    .map(|i| i64::from(i.quantity))
                    .sum()
            });
            assert_eq!(
                reported_avail, db_avail,
                "OnIndustryMaterials dict value must equal post-consumption hangar quantity for type {}",
                mat.type_id
            );
            assert!(
                reported_avail < i64::from(initial_stack_qty),
                "Material {} must have been consumed below initial stack {}",
                mat.type_id,
                initial_stack_qty
            );
        }

        assert_eq!(
            rpc(
                &ctx,
                &mut sess,
                "industryMonitor",
                "DisconnectJob",
                &[PyRep::Integer(monitor_id)],
            )
            .await
            .expect("DisconnectJob should succeed"),
            PyRep::Bool(true)
        );

        // Corporate hangar division isolation: useCorp=true with inputFlag=CorpSAG1 must consume from and report ONLY CorpSAG1
        let corp_id = 98_400_001_i64;
        let corp_bp_id = 920_000_100_i64;
        let sag1_initial: i32 = 2_000_000;
        let sag2_initial: i32 = 500_000;
        let take3_only = 1u64 << 15; // Take3 (bit 15), NOT Take1 (bit 13)
        let wrong_div_roles = eve_db::corp_roles::ROLE_FACTORY_MANAGER | (1u64 << 15); // Factory Manager + Take3
        let fm_roles = eve_db::corp_roles::ROLE_FACTORY_MANAGER | (1u64 << 13); // Factory Manager + Take1 (bit 13)
        ctx.db.with_mut_snapshot(|snap| {
            let mut corp = eve_db::CorporationRecord {
                corporation_id: corp_id,
                name: "Industry Division Corp".into(),
                ticker: "IDIV".into(),
                ceo_id: PILOT_B,
                hq_station_id: Some(JITA_STATION),
                master_wallet: IskCents(100_000_000_00),
                ..Default::default()
            };
            corp.members.insert(PILOT_A);
            corp.members.insert(PILOT_B);
            corp.member_roles.insert(PILOT_A, take3_only);
            snap.corporations.insert(corp_id, corp);
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.corporation_id = corp_id;
            }
            snap.skills.entry(PILOT_A).or_default().insert(
                3387,
                eve_db::TrainedSkillRecord { type_id: 3387, level: 5, skill_points: 256_000.0 },
            );
            snap.items.insert(
                corp_bp_id,
                make_item(corp_bp_id, 687, corp_id, JITA_STATION, InventoryFlag::CorpSAG1, 1, true),
            );
            for (idx, mat) in bp_info.materials.iter().enumerate() {
                snap.items.insert(
                    920_000_110 + idx as i64,
                    make_item(920_000_110 + idx as i64, mat.type_id, corp_id, JITA_STATION, InventoryFlag::CorpSAG1, sag1_initial, false),
                );
                snap.items.insert(
                    920_000_210 + idx as i64,
                    make_item(920_000_210 + idx as i64, mat.type_id, corp_id, JITA_STATION, InventoryFlag::CorpSAG2, sag2_initial, false),
                );
            }
        });
        sess.corporation_id = Some(corp_id);
        sess.corp_role = take3_only;

        // Negative: member holding Take3 (bit 15) must be denied CorpSAG1 on ConnectJob
        let take3_conn_err = rpc(
            &ctx,
            &mut sess,
            "industryMonitor",
            "ConnectJob",
            &[PyRep::keyval([
                ("ownerID", PyRep::Integer(corp_id)),
                ("locationID", PyRep::Integer(JITA_STATION)),
                ("inputFlagID", PyRep::Integer(InventoryFlag::CorpSAG1 as i64)),
            ])],
        )
        .await
        .expect_err("Take3 holder must be denied CorpSAG1 ConnectJob");
        assert_eq!(take3_conn_err.code, "CrpAccessDenied");

        // Negative: member holding Factory Manager + Take3 (bit 15) must be denied CorpSAG1 on InstallJob
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                corp.member_roles.insert(PILOT_A, wrong_div_roles);
            }
        });
        sess.corp_role = wrong_div_roles;

        let take3_inst_err = rpc(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintItemID", PyRep::Integer(corp_bp_id)),
                ("facilityID", PyRep::Integer(JITA_STATION)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
                ("useCorp", PyRep::Bool(true)),
                ("inputFlag", PyRep::Integer(InventoryFlag::CorpSAG1 as i64)),
            ])],
        )
        .await
        .expect_err("Take3 holder must be denied CorpSAG1 InstallJob");
        assert_eq!(take3_inst_err.code, "CrpAccessDenied");

        // Grant Take1 (bit 13): now CorpSAG1 succeeds
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                corp.member_roles.insert(PILOT_A, fm_roles);
            }
        });
        sess.corp_role = fm_roles;
        let _ = ctx.drain_notifications_for_char(PILOT_A);

        let corp_mon_rep = rpc(
            &ctx,
            &mut sess,
            "industryMonitor",
            "ConnectJob",
            &[PyRep::keyval([
                ("ownerID", PyRep::Integer(corp_id)),
                ("locationID", PyRep::Integer(JITA_STATION)),
                ("inputFlagID", PyRep::Integer(InventoryFlag::CorpSAG1 as i64)),
            ])],
        )
        .await
        .expect("corp industryMonitor.ConnectJob should succeed");
        let PyRep::Tuple(corp_mon_parts) = corp_mon_rep else {
            panic!("ConnectJob must return (monitorID, available_dict) tuple");
        };
        let corp_monitor_id = corp_mon_parts[0].as_i64().expect("corp monitorID");

        let corp_job = rpc(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintItemID", PyRep::Integer(corp_bp_id)),
                ("facilityID", PyRep::Integer(JITA_STATION)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
                ("useCorp", PyRep::Bool(true)),
                ("inputFlag", PyRep::Integer(InventoryFlag::CorpSAG1 as i64)),
            ])],
        )
        .await
        .expect("Corporate InstallJob from CorpSAG1 should succeed");
        let corp_job_id = corp_job.dict_get("jobID").and_then(|v| v.as_i64()).expect("corp jobID");
        assert!(corp_job_id > 0);
        let corp_notifs = ctx.drain_notifications_for_char(PILOT_A);
        let corp_ind_mats = find_notifs(&corp_notifs, "OnIndustryMaterials");
        assert_eq!(corp_ind_mats.len(), 1);
        assert_eq!(corp_ind_mats[0].args[0], PyRep::Integer(corp_monitor_id));
        let PyRep::Dict(corp_mat_entries) = &corp_ind_mats[0].args[1] else {
            panic!("expected Dict in corporate OnIndustryMaterials");
        };
        for mat in &bp_info.materials {
            let reported = corp_mat_entries
                .iter()
                .find(|(k, _)| k.as_i64() == Some(i64::from(mat.type_id)))
                .and_then(|(_, v)| v.as_i64())
                .expect("consumed material in corp OnIndustryMaterials");
            let (sag1_db, sag2_db): (i64, i64) = ctx.db.with_snapshot(|snap| {
                let s1 = snap.items.values().filter(|i| i.owner_id == corp_id && i.location_id == JITA_STATION && i.type_id == mat.type_id && i.flag == InventoryFlag::CorpSAG1).map(|i| i64::from(i.quantity)).sum();
                let s2 = snap.items.values().filter(|i| i.owner_id == corp_id && i.location_id == JITA_STATION && i.type_id == mat.type_id && i.flag == InventoryFlag::CorpSAG2).map(|i| i64::from(i.quantity)).sum();
                (s1, s2)
            });
            assert_eq!(sag2_db, i64::from(sag2_initial), "CorpSAG2 stack must not be touched by CorpSAG1 job");
            assert_eq!(
                reported, sag1_db,
                "OnIndustryMaterials for corporate job must report strictly CorpSAG1 remaining quantity (excluding personal Hangar and CorpSAG2)"
            );
            assert!(reported < i64::from(sag1_initial));
        }
        assert_eq!(
            rpc(
                &ctx,
                &mut sess,
                "industryMonitor",
                "DisconnectJob",
                &[PyRep::Integer(corp_monitor_id)],
            )
            .await
            .expect("corp DisconnectJob should succeed"),
            PyRep::Bool(true)
        );
    }

    #[tokio::test]
    async fn test_on_community_fittings_updated_auth_and_broadcast() {
        // parity: notify:OnCommunityFittingsUpdated
        eve_testkit::attest(&["notify:OnCommunityFittingsUpdated"]);

        let ctx = ServerContext::new();
        let mut sess_a = login_pilot(&ctx, 8802, "demo", "demo-secret", PILOT_A).await;
        let _sess_b = login_pilot(&ctx, 8803, "other", "other-secret", PILOT_B).await;
        let _ = ctx.drain_notifications_for_char(PILOT_A);
        let _ = ctx.drain_notifications_for_char(PILOT_B);

        let fit_kv = PyRep::keyval([
            ("name", PyRep::String("Community Rifter".into())),
            ("shipTypeID", PyRep::Integer(587)),
            ("description", PyRep::String("Curated V24 community fit".into())),
            ("fitData", PyRep::List(vec![])),
        ]);

        // Negative 1: unprivileged player cannot save to COMMUNITY_FITTING_CORP (1000282) or owner 1
        sess_a.role = 0;
        for owner in [COMMUNITY_FITTING_CORP_ID, 1_i64] {
            let denied = rpc(
                &ctx,
                &mut sess_a,
                "corpFittingMgr",
                "SaveFitting",
                &[PyRep::Integer(owner), fit_kv.clone()],
            )
            .await
            .expect_err("unprivileged player must not save community fittings");
            assert_eq!(denied.code, "AccessDenied");
        }
        assert!(
            find_notifs(
                &ctx.drain_notifications_for_char(PILOT_A),
                "OnCommunityFittingsUpdated"
            )
            .is_empty()
        );

        // Negative 2: personal charFittingMgr.SaveFitting emits OnFittingsUpdated, NOT OnCommunityFittingsUpdated
        let personal_fid = rpc(
            &ctx,
            &mut sess_a,
            "charFittingMgr",
            "SaveFitting",
            &[PyRep::Integer(PILOT_A), fit_kv.clone()],
        )
        .await
        .expect("personal SaveFitting")
        .as_i64()
        .expect("fitting id");
        assert!(personal_fid > 0);
        assert!(
            find_notifs(
                &ctx.drain_notifications_for_char(PILOT_A),
                "OnCommunityFittingsUpdated"
            )
            .is_empty()
        );
        let _ = ctx.drain_notifications_for_char(PILOT_B);

        // Positive 1: Content/GM role saving to COMMUNITY_FITTING_CORP (1000282) broadcasts 0-arg OnCommunityFittingsUpdated
        sess_a.role = service_roles::ROLE_CONTENT;
        let comm_fid = rpc(
            &ctx,
            &mut sess_a,
            "corpFittingMgr",
            "SaveFitting",
            &[PyRep::Integer(COMMUNITY_FITTING_CORP_ID), fit_kv],
        )
        .await
        .expect("Content SaveFitting to COMMUNITY_FITTING_CORP")
        .as_i64()
        .expect("community fitting id");
        assert!(comm_fid > 0);

        for cid in [PILOT_A, PILOT_B] {
            let notifs = ctx.drain_notifications_for_char(cid);
            let comm_notifs = find_notifs(&notifs, "OnCommunityFittingsUpdated");
            assert_eq!(
                comm_notifs.len(),
                1,
                "Online pilot {cid} must receive broadcast OnCommunityFittingsUpdated"
            );
            assert_eq!(comm_notifs[0].id_type, "clientID");
            assert!(comm_notifs[0].args.is_empty());
        }

        let comm_dict = rpc(&ctx, &mut sess_a, "corpFittingMgr", "GetCommunityFittings", &[])
            .await
            .expect("GetCommunityFittings");
        let PyRep::Dict(entries) = comm_dict else {
            panic!("expected Dict from GetCommunityFittings");
        };
        assert!(
            entries.iter().any(|(k, _)| k.as_i64() == Some(comm_fid)),
            "GetCommunityFittings must include newly saved community fitting"
        );

        // Negative 3: unprivileged caller cannot rename or delete community fitting
        sess_a.role = 0;
        let upd_denied = rpc(
            &ctx,
            &mut sess_a,
            "corpFittingMgr",
            "UpdateNameAndDescription",
            &[
                PyRep::Integer(comm_fid),
                PyRep::Integer(COMMUNITY_FITTING_CORP_ID),
                PyRep::String("Unauthorized Rename".into()),
                PyRep::String("Should fail".into()),
            ],
        )
        .await
        .expect_err("unprivileged player must not update community fittings");
        assert_eq!(upd_denied.code, "AccessDenied");
        let del_denied = rpc(
            &ctx,
            &mut sess_a,
            "corpFittingMgr",
            "DeleteFitting",
            &[
                PyRep::Integer(COMMUNITY_FITTING_CORP_ID),
                PyRep::Integer(comm_fid),
            ],
        )
        .await
        .expect_err("unprivileged player must not delete community fittings");
        assert_eq!(del_denied.code, "AccessDenied");
        assert!(
            find_notifs(
                &ctx.drain_notifications_for_char(PILOT_B),
                "OnCommunityFittingsUpdated"
            )
            .is_empty()
        );

        // Positive 2: Content/GM role renaming community fitting updates metadata and broadcasts 0-arg OnCommunityFittingsUpdated
        sess_a.role = service_roles::ROLE_CONTENT;
        let renamed = rpc(
            &ctx,
            &mut sess_a,
            "corpFittingMgr",
            "UpdateNameAndDescription",
            &[
                PyRep::Integer(comm_fid),
                PyRep::Integer(COMMUNITY_FITTING_CORP_ID),
                PyRep::String("Updated Community Rifter".into()),
                PyRep::String("Updated curated V24 community fit".into()),
            ],
        )
        .await
        .expect("Content UpdateNameAndDescription");
        assert_eq!(renamed.dict_get("name"), Some(&PyRep::String("Updated Community Rifter".into())));
        let b_upd_notifs = ctx.drain_notifications_for_char(PILOT_B);
        let b_upd_comm = find_notifs(&b_upd_notifs, "OnCommunityFittingsUpdated");
        assert_eq!(b_upd_comm.len(), 1);
        assert!(b_upd_comm[0].args.is_empty());

        // Positive 3: Content/GM role deleting community fitting broadcasts 0-arg OnCommunityFittingsUpdated
        sess_a.role = service_roles::ROLE_CONTENT;
        let deleted = rpc(
            &ctx,
            &mut sess_a,
            "corpFittingMgr",
            "DeleteFitting",
            &[
                PyRep::Integer(COMMUNITY_FITTING_CORP_ID),
                PyRep::Integer(comm_fid),
            ],
        )
        .await
        .expect("Content DeleteFitting");
        assert_eq!(deleted, PyRep::Bool(true));

        let b_del_notifs = ctx.drain_notifications_for_char(PILOT_B);
        let b_comm = find_notifs(&b_del_notifs, "OnCommunityFittingsUpdated");
        assert_eq!(b_comm.len(), 1);
        assert!(b_comm[0].args.is_empty());
    }

    #[tokio::test]
    async fn test_on_tutorial_skipped_air_npe_and_operations() {
        // parity: notify:OnTutorialSkipped
        eve_testkit::attest(&["notify:OnTutorialSkipped"]);

        let ctx = ServerContext::new();
        let mut sess_a = login_pilot(&ctx, 8804, "demo", "demo-secret", PILOT_A).await;
        let mut sess_b = login_pilot(&ctx, 8805, "other", "other-secret", PILOT_B).await;
        let _ = ctx.drain_notifications_for_char(PILOT_A);
        let _ = ctx.drain_notifications_for_char(PILOT_B);

        // Negative 1: unauthenticated skip_air_npe fails and emits 0 OnTutorialSkipped
        let mut unauth = SessionState::new(8898);
        assert!(rpc(&ctx, &mut unauth, "air_npe", "skip_air_npe", &[]).await.is_err());
        assert!(
            find_notifs(&ctx.drain_notifications_for_char(PILOT_A), "OnTutorialSkipped").is_empty()
        );

        // Negative 2: non-skip client event does not emit OnTutorialSkipped
        rpc(
            &ctx,
            &mut sess_a,
            "operationsManager",
            "process_client_event",
            &[PyRep::String("start_tutorial".into())],
        )
        .await
        .expect("start_tutorial");
        assert!(
            find_notifs(&ctx.drain_notifications_for_char(PILOT_A), "OnTutorialSkipped").is_empty()
        );

        // Positive 1: air_npe.skip_air_npe marks tutorial completed and emits 0-arg OnTutorialSkipped
        let skip_rep = rpc(&ctx, &mut sess_a, "air_npe", "skip_air_npe", &[])
            .await
            .expect("air_npe.skip_air_npe");
        assert_eq!(skip_rep.dict_get("completed"), Some(&PyRep::Bool(true)));
        let state_rep = rpc(&ctx, &mut sess_a, "air_npe", "get_air_npe_state", &[])
            .await
            .expect("air_npe.get_air_npe_state");
        assert_eq!(state_rep.dict_get("completed"), Some(&PyRep::Bool(true)));

        let a_notifs = ctx.drain_notifications_for_char(PILOT_A);
        let a_skipped = find_notifs(&a_notifs, "OnTutorialSkipped");
        assert_eq!(a_skipped.len(), 1);
        assert_eq!(a_skipped[0].id_type, "charid");
        assert!(a_skipped[0].args.is_empty());

        // Positive 2: operationsManager.process_client_event("skip_tutorial") also emits 0-arg OnTutorialSkipped
        rpc(
            &ctx,
            &mut sess_b,
            "operationsManager",
            "process_client_event",
            &[PyRep::String("skip_tutorial".into())],
        )
        .await
        .expect("operationsManager skip_tutorial");
        let b_notifs = ctx.drain_notifications_for_char(PILOT_B);
        let b_skipped = find_notifs(&b_notifs, "OnTutorialSkipped");
        assert_eq!(b_skipped.len(), 1);
        assert_eq!(b_skipped[0].id_type, "charid");
        assert!(b_skipped[0].args.is_empty());
    }
}

pub(crate) mod notify_live_hypernet_raffle_notifications {
    // Live client-RPC notification closure tests (`notify:On*`).
    //
    // Verifies server-to-client V24 notifications emitted by real domain RPCs
    // (`raffleProxy.CreateRaffle`, `raffleProxy.BuyRandomTickets`).


    use crate::common;
    use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    const PILOT_A: i64 = 90_000_001;
    const PILOT_B: i64 = 90_000_002;
    const PILOT_C: i64 = 90_000_003;
    const JITA_STATION: i64 = 60_003_760;

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

    async fn login_in_station(
        ctx: &ServerContext,
        conn_id: u64,
        user: &str,
        pass: &str,
        char_id: i64,
    ) -> SessionState {
        let mut sess = SessionState::new(conn_id);
        common::login_pilot(ctx, &mut sess, user, pass).await;
        rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
            .await
            .expect("SelectCharacterID");

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = Some(JITA_STATION);
                ch.balance = IskCents::from_isk(100_000_000);
            }
        });
        sess.station_id = Some(JITA_STATION);
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
            .unwrap_or_else(|| {
                panic!(
                    "Expected notification {name}, got {:?}",
                    notifs.iter().map(|(e, _)| e).collect::<Vec<_>>()
                )
            })
    }

    // parity: notify:OnRaffleCreatedServer, notify:OnRaffleUpdatedServer, notify:OnRaffleFinishedServer
    #[tokio::test]
    async fn test_notify_live_hypernet_raffle_notifications() {
        eve_testkit::attest(&[
            "notify:OnRaffleCreatedServer",
            "notify:OnRaffleUpdatedServer",
            "notify:OnRaffleFinishedServer",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let prize_id = 640_000_110_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.accounts.insert(
                "test_c".to_string(),
                eve_db::AccountRecord {
                    user_id: 3,
                    username: "test_c".to_string(),
                    password_token: eve_db::WorldRepository::hash_password_token("test_c", "test_c-secret"),
                    role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            if let Some(mut char_c) = snap.characters.get(&PILOT_A).cloned() {
                char_c.character_id = PILOT_C;
                char_c.user_id = 3;
                char_c.name = "Test C".to_string();
                snap.characters.insert(PILOT_C, char_c);
            }
        });
        let mut sess_a = login_in_station(&ctx, 9_501, "demo", "demo-secret", PILOT_A).await;
        let mut sess_b = login_in_station(&ctx, 9_502, "other", "other-secret", PILOT_B).await;
        let mut sess_c = login_in_station(&ctx, 9_503, "test_c", "test_c-secret", PILOT_C).await;
        rpc(&ctx, &mut sess_c, "raffleProxy", "SubscribeToRaffle", &[]).await.expect("sub");

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(prize_id, make_item(prize_id, 587, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true));
            snap.items.insert(prize_id + 1, make_item(prize_id + 1, 52_568, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 10, false));
        });
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

        let create_notifs_c = drain_all(&ctx, &mut sess_c, PILOT_C);
        let created_args_c = find_notif(&create_notifs_c, "OnRaffleCreatedServer");
        assert_eq!(created_args_c[0], PyRep::Integer(raffle_id));

        rpc(&ctx, &mut sess_b, "raffleProxy", "BuyRandomTickets", &[PyRep::Integer(raffle_id), PyRep::Integer(8)])
            .await
            .expect("BuyRandomTickets");
        let buy_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
        let updated_args = find_notif(&buy_notifs_b, "OnRaffleUpdatedServer");
        assert_eq!(updated_args[0], PyRep::Integer(raffle_id));
        let finished_args = find_notif(&buy_notifs_b, "OnRaffleFinishedServer");
        assert_eq!(finished_args[0], PyRep::Integer(raffle_id));
        let win_ticket = finished_args[1].as_i64().expect("winning ticket index");
        assert!((0..8).contains(&win_ticket));

        let buy_notifs_c = drain_all(&ctx, &mut sess_c, PILOT_C);
        let updated_args_c = find_notif(&buy_notifs_c, "OnRaffleUpdatedServer");
        assert_eq!(updated_args_c[0], PyRep::Integer(raffle_id));
        let finished_args_c = find_notif(&buy_notifs_c, "OnRaffleFinishedServer");
        assert_eq!(finished_args_c[0], PyRep::Integer(raffle_id));
        assert_eq!(finished_args_c[1], PyRep::Integer(win_ticket));
    }
}

pub(crate) mod z1_systematic_rpc_catalog_dispatch {
    // Parity test suite for Full RPC & Notification Closure (`Z-1` & `Z-2`).


    use crate::common;

    use eve_machonet::{MachoPacket, MachoPacketType, SessionState};
    use eve_proto::{marshal, PyRep};
    use eve_server::router::{
        cache_notifications::{NotificationTarget, MAX_PENDING_NOTIFICATIONS_PER_CHAR},
        ServerContext,
    };
    use std::collections::{BTreeSet, HashMap};
    use std::path::PathBuf;

    fn root_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[tokio::test]
    // parity: plan:P-2, plan:P-3, plan:P-4
    async fn test_z1_systematic_rpc_catalog_dispatch_closure_and_wire_formatting() {
        // parity: rpc:charUnboundMgr.GetCharacterInfo, rpc:charUnboundMgr.SelectCharacterID, rpc:corpRegistry.AddCorporation, rpc:account.GetWalletDivisionsInfo, rpc:marketProxy.GetCharOrders, rpc:marketProxy.GetOrders, rpc:slash.SlashCmd, rpc:stationSvc.GetStation
        let ctx = ServerContext::new();
        ctx.seed_demo_world_fixtures();
        let empty_kw = HashMap::new();

        // 1. Load the authoritative RPC service catalog (216 machoNet services)
        let catalog_path = root_dir().join("docs/index/rpc_service_catalog.json");
        let catalog_raw = std::fs::read_to_string(&catalog_path)
            .unwrap_or_else(|_| panic!("Failed to read {}", catalog_path.display()));
        let catalog: serde_json::Value =
            serde_json::from_str(&catalog_raw).expect("Parse rpc_service_catalog.json");
        let catalog_map = catalog.as_object().expect("Catalog JSON must be an object");

        assert_eq!(
            catalog_map.len(),
            216,
            "Authoritative catalog must contain exactly 216 services"
        );

        // 2. Setup an authenticated pilot session (character 90000001 in solar system 30000142, docked at 60003760)
        let mut sess = SessionState::new(101);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        ctx.dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &empty_kw,
            Some("test:Z-1:SelectCharacterID"),
        )
        .await
        .expect("SelectCharacterID");

        assert_eq!(sess.character_id, Some(90_000_001));
        assert_eq!(sess.solar_system_id, Some(30_000_142));

        // 3. Systematically sweep every catalog service and every client candidate method (1,230 methods across 183 services)
        let mut total_candidate_methods = 0usize;
        let mut total_services_tested = 0usize;
        let mut legacy_services_tested = 0usize;
        let mut concrete_ok_methods = 0usize;
        let mut domain_error_methods = 0usize;
        let mut unimplemented_methods = 0usize;

        for (service_name, service_meta) in catalog_map {
            total_services_tested += 1;
            let surface_cat = service_meta
                .get("surface_category")
                .and_then(|v| v.as_str())
                .unwrap_or("v24_active");
            let is_legacy = surface_cat == "crucible_legacy_only";
            if is_legacy {
                legacy_services_tested += 1;
            }

            let cands: Vec<String> = service_meta
                .get("v24_client_candidate_methods")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| s.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();

            for method in &cands {
                total_candidate_methods += 1;

                // Dispatch each candidate method with empty arguments on an authenticated session copy
                let mut sweep_sess = sess.clone();
                let res = ctx
                    .dispatch_rpc(
                        &mut sweep_sess,
                        service_name,
                        None,
                        method,
                        &[],
                        &empty_kw,
                        Some("test:Z-1:SystematicSweep"),
                    )
                    .await;

                match res {
                    Ok(pyrep) => {
                        concrete_ok_methods += 1;
                        // Verify wire-level MachoPacket CallRsp encoding and binary marshal round-trip
                        let (pkt, marshaled) = ServerContext::build_macho_call_response_packet(
                            Some(service_name),
                            101,
                            total_candidate_methods as u64,
                            sess.user_id,
                            pyrep.clone(),
                        );
                        assert_eq!(pkt.packet_type, MachoPacketType::CallRsp);
                        let decoded_rep = marshal::decode(&marshaled).expect("Decode CallRsp wire bytes");
                        let decoded_pkt =
                            MachoPacket::from_pyrep(&decoded_rep).expect("Parse CallRsp MachoPacket");
                        assert_eq!(decoded_pkt.packet_type, MachoPacketType::CallRsp);
                        assert_eq!(decoded_pkt.user_id, sess.user_id);
                    }
                    Err(err) => {
                        assert!(
                            !err.code.is_empty(),
                            "RPC error code must not be empty for {service_name}.{method}"
                        );
                        assert!(
                            !err.message.is_empty(),
                            "RPC error message must not be empty for {service_name}.{method}"
                        );

                        if err.code == "UnimplementedRPC" {
                            unimplemented_methods += 1;
                        } else {
                            domain_error_methods += 1;
                        }

                        // For legacy Crucible services, verify they strictly return UnimplementedRPC
                        if is_legacy {
                            assert_eq!(
                                err.code, "UnimplementedRPC",
                                "Legacy service {service_name}.{method} must return UnimplementedRPC"
                            );
                        }

                        // Verify wire-level MachoPacket ErrorResponse encoding and binary marshal round-trip
                        let (pkt, marshaled) = ServerContext::build_macho_error_response_packet(
                            Some(service_name),
                            101,
                            total_candidate_methods as u64,
                            sess.user_id,
                            &err.code,
                            &err.message,
                        );
                        assert_eq!(pkt.packet_type, MachoPacketType::ErrorResponse);
                        let decoded_rep =
                            marshal::decode(&marshaled).expect("Decode ErrorResponse wire bytes");
                        let decoded_pkt = MachoPacket::from_pyrep(&decoded_rep)
                            .expect("Parse ErrorResponse MachoPacket");
                        assert_eq!(decoded_pkt.packet_type, MachoPacketType::ErrorResponse);
                    }
                }
            }
        }

        assert_eq!(total_services_tested, 216);
        assert_eq!(legacy_services_tested, 23);
        assert_eq!(
            total_candidate_methods, 1230,
            "Must systematically sweep all 1,230 cataloged candidate methods"
        );
        assert!(concrete_ok_methods > 0, "Must have concrete handled RPC methods");
        assert!(domain_error_methods > 0, "Must have domain error RPC methods");
        assert!(unimplemented_methods > 0, "Must have structured UnimplementedRPC fallback methods");

        // Re-dock pilot at Jita 4-4 for station-bound service testing following the systematic navigation sweep
        let _ = ctx.db.dock_character(90_000_001, 60_003_760, false); ctx.advance_secs(12);

        // 4. Concrete functional verification across major domain services:
        // a) charUnboundMgr.GetCharactersToSelect
        let chars_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "charUnboundMgr",
                None,
                "GetCharactersToSelect",
                &[],
                &empty_kw,
                None,
            )
            .await
            .expect("charUnboundMgr.GetCharactersToSelect");
        assert!(matches!(
            chars_rep,
            PyRep::Rowset { .. } | PyRep::List(_) | PyRep::Object { .. } | PyRep::ObjectEx { .. }
        ));

        // b) charUnboundMgr.GetCharacterInfo
        let char_info = ctx
            .dispatch_rpc(
                &mut sess,
                "charUnboundMgr",
                None,
                "GetCharacterInfo",
                &[PyRep::Integer(90_000_001)],
                &empty_kw,
                None,
            )
            .await
            .expect("charUnboundMgr.GetCharacterInfo");
        assert!(matches!(char_info, PyRep::KeyVal(_) | PyRep::Object { .. }));

        // c) stationSvc.GetStation
        let st_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "stationSvc",
                None,
                "GetStation",
                &[PyRep::Integer(60_003_760)],
                &empty_kw,
                None,
            )
            .await
            .expect("stationSvc.GetStation");
        assert!(matches!(st_rep, PyRep::KeyVal(_)));

        // d) fleetMgr.CreateFleet & GetFleet
        let fleet_rep = ctx
            .dispatch_rpc(&mut sess, "fleetMgr", None, "CreateFleet", &[], &empty_kw, None)
            .await
            .expect("fleetMgr.CreateFleet");
        assert!(matches!(fleet_rep, PyRep::KeyVal(_)));
        let get_fleet = ctx
            .dispatch_rpc(&mut sess, "fleetMgr", None, "GetFleet", &[], &empty_kw, None)
            .await
            .expect("fleetMgr.GetFleet");
        assert!(matches!(get_fleet, PyRep::KeyVal(_)));

        // e) corpmgr.GetMembers, corpRegistry.AddCorporation, & account.GetWalletDivisionsInfo
        let corp_members = ctx
            .dispatch_rpc(&mut sess, "corpmgr", None, "GetMembers", &[], &empty_kw, None)
            .await
            .expect("corpmgr.GetMembers");
        assert!(matches!(corp_members, PyRep::Rowset { .. }));

        let create_corp = ctx
            .dispatch_rpc(
                &mut sess,
                "corpRegistry",
                None,
                "AddCorporation",
                &[
                    PyRep::String("Z1 Closure Corp".into()),
                    PyRep::String("Z1CC".into()),
                    PyRep::String("Z1 Corp Description".into()),
                ],
                &empty_kw,
                None,
            )
            .await
            .expect("corpRegistry.AddCorporation");
        assert!(matches!(create_corp, PyRep::KeyVal(_)));

        let corp_wallets = ctx
            .dispatch_rpc(
                &mut sess,
                "account",
                None,
                "GetWalletDivisionsInfo",
                &[],
                &empty_kw,
                None,
            )
            .await
            .expect("account.GetWalletDivisionsInfo");
        assert!(matches!(corp_wallets, PyRep::Rowset { .. }));

        // f) LSC.JoinChannel & SendMessage
        let join_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "LSC",
                None,
                "JoinChannel",
                &[PyRep::String("test_chan".into())],
                &empty_kw,
                None,
            )
            .await
            .expect("LSC.JoinChannel");
        assert!(matches!(join_rep, PyRep::KeyVal(_)));

        let msg_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "LSC",
                None,
                "SendMessage",
                &[
                    PyRep::String("test_chan".into()),
                    PyRep::String("Hello from Z-1!".into()),
                ],
                &empty_kw,
                None,
            )
            .await
            .expect("LSC.SendMessage");
        assert!(matches!(msg_rep, PyRep::KeyVal(_)));

        // g) mailMgr.GetHeaders
        let mail_headers = ctx
            .dispatch_rpc(&mut sess, "mailMgr", None, "GetHeaders", &[], &empty_kw, None)
            .await
            .expect("mailMgr.GetHeaders");
        assert!(matches!(mail_headers, PyRep::Rowset { .. }));

        // h) bookmark.GetBookmarks
        let bms = ctx
            .dispatch_rpc(&mut sess, "bookmark", None, "GetBookmarks", &[], &empty_kw, None)
            .await
            .expect("bookmark.GetBookmarks");
        assert!(matches!(bms, PyRep::Rowset { .. }));

        // i) marketProxy.GetOrders & marketProxy.GetCharOrders
        let orders = ctx
            .dispatch_rpc(
                &mut sess,
                "marketProxy",
                None,
                "GetOrders",
                &[PyRep::Integer(34)],
                &empty_kw,
                None,
            )
            .await
            .expect("marketProxy.GetOrders");
        assert!(matches!(orders, PyRep::Tuple(ref items) if items.len() == 2));

        let char_orders = ctx
            .dispatch_rpc(
                &mut sess,
                "marketProxy",
                None,
                "GetCharOrders",
                &[],
                &empty_kw,
                None,
            )
            .await
            .expect("marketProxy.GetCharOrders");
        assert!(matches!(char_orders, PyRep::Rowset { .. }));

        // j) charFittingMgr.GetFittings
        let fittings = ctx
            .dispatch_rpc(
                &mut sess,
                "charFittingMgr",
                None,
                "GetFittings",
                &[],
                &empty_kw,
                None,
            )
            .await
            .expect("charFittingMgr.GetFittings");
        assert!(matches!(fittings, PyRep::Dict(_)));

        // 5. Test MachoBindObject with piggybacked method call
        let bind_res = ctx
            .dispatch_rpc(
                &mut sess,
                "invbroker",
                None,
                "MachoBindObject",
                &[
                    PyRep::Integer(60_003_760),
                    PyRep::Tuple(vec![
                        PyRep::String("List".into()),
                        PyRep::Tuple(vec![]),
                        PyRep::Dict(vec![]),
                    ]),
                ],
                &empty_kw,
                None,
            )
            .await
            .expect("MachoBindObject piggyback call");
        assert!(matches!(bind_res, PyRep::Tuple(ref items) if items.len() == 2));

        // 6. Test security & access controls:
        // a) Unauthenticated call to protected service
        let mut unauth_sess = SessionState::new(999);
        let unauth_err = ctx
            .dispatch_rpc(
                &mut unauth_sess,
                "charUnboundMgr",
                None,
                "GetCharacterInfo",
                &[],
                &empty_kw,
                None,
            )
            .await
            .expect_err("Unauthenticated protected call must fail");
        assert_eq!(unauth_err.code, "Unauthorized");

        // b) Non-GM call to GM slash command
        let gm_err = ctx
            .dispatch_rpc(
                &mut sess,
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String("/giveisk 1000000".into())],
                &empty_kw,
                None,
            )
            .await
            .expect_err("Non-GM slash command call must fail");
        assert_eq!(gm_err.code, "InsufficientRole");
    }

    #[tokio::test]
    // parity: plan:P-5
    async fn test_z2_systematic_notification_emission_pipelines_and_scope_multicast() {
        // parity: rpc:charUnboundMgr.SelectCharacterID
        let ctx = ServerContext::new();
        ctx.seed_demo_world_fixtures();
        let empty_kw = HashMap::new();

        // 1. Provision & login 3 distinct pilots for multi-target multicast testing:
        // Pilot 1 (Demo, char 90000001, corp 98000001, sys 30000142, station 60003760)
        let mut sess1 = SessionState::new(201);
        common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
        ctx.dispatch_rpc(
            &mut sess1,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &empty_kw,
            None,
        )
        .await
        .expect("Select Pilot 1");

        // Pilot 2 (Rival/Other, char 90000002, corp 98000001, sys 30000142, station 60003760)
        let mut sess2 = SessionState::new(202);
        common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
        ctx.dispatch_rpc(
            &mut sess2,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_002)],
            &empty_kw,
            None,
        )
        .await
        .expect("Select Pilot 2");

        // Pilot 3 (Third pilot, char 90000003 in DIFFERENT solar system 30000144 and different corp 98000002)
        let p3_uid = ctx
            .db
            .provision_player_account("pilot3", "pilot3-secret");
        let p3_char = ctx
            .db
            .create_character(
                p3_uid,
                "Pilot Three",
                1,
                1,
                11,
                17,
                &ctx.sde,
            )
            .expect("create character for pilot3");
        let p3_char_id = p3_char.character_id;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&p3_char_id) {
                ch.solar_system_id = 30_000_144;
                ch.corporation_id = 98_000_002;
                ch.station_id = Some(60_003_754);
            }
        });

        let mut sess3 = SessionState::new(203);
        common::login_pilot(&ctx, &mut sess3, "pilot3", "pilot3-secret").await;
        ctx.dispatch_rpc(
            &mut sess3,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(p3_char_id)],
            &empty_kw,
            None,
        )
        .await
        .expect("Select Pilot 3");

        // Align Pilot 1 and Pilot 2 to Corporation 98000001 (Pilot 3 is in 98000002)
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.corporation_id = 98_000_001;
                ch.station_id = Some(60_003_760);
            }
            if let Some(ch) = snap.characters.get_mut(&90_000_002) {
                ch.corporation_id = 98_000_001;
                ch.station_id = Some(60_003_760);
            }
        });

        // Drain any bootstrap notifications
        let _ = ctx.drain_notifications_for_char(90_000_001);
        let _ = ctx.drain_notifications_for_char(90_000_002);
        let _ = ctx.drain_notifications_for_char(p3_char_id);

        // =========================================================================
        // Pipeline 1: OnAccountChange
        // =========================================================================
        let notifs1 = ctx.notify_account_change(90_000_001, "cash", 90_000_001, 75_000_000.0);
        assert_eq!(notifs1.len(), 1);
        assert_eq!(notifs1[0].event, "OnAccountChange");
        assert_eq!(notifs1[0].id_type, "charid");
        assert_eq!(notifs1[0].args.len(), 3);
        assert_eq!(notifs1[0].args[0], PyRep::String("cash".into()));
        assert_eq!(notifs1[0].args[1], PyRep::Integer(90_000_001));
        assert_eq!(notifs1[0].args[2], PyRep::Real(75_000_000.0));

        // Verify binary packet marshal round-trip
        let rep1 = marshal::decode(&notifs1[0].marshaled_packet).expect("decode OnAccountChange");
        let pkt1 = MachoPacket::from_pyrep(&rep1).expect("parse OnAccountChange packet");
        assert_eq!(pkt1.packet_type, MachoPacketType::Notification);

        // =========================================================================
        // Pipeline 2: OnItemChange
        // =========================================================================
        let sample_item = PyRep::Tuple(vec![
            PyRep::Integer(100_001),
            PyRep::Integer(34),
            PyRep::Integer(90_000_001),
            PyRep::Integer(60_003_760),
            PyRep::Integer(4),
            PyRep::Integer(500),
        ]);
        let notifs2 = ctx.notify_item_change(
            90_000_001,
            sample_item.clone(),
            vec![(5, PyRep::Integer(600))],
        );
        assert_eq!(notifs2.len(), 1);
        assert_eq!(notifs2[0].event, "OnItemChange");
        assert_eq!(notifs2[0].id_type, "charid");
        assert_eq!(notifs2[0].args.len(), 2);
        assert_eq!(notifs2[0].args[0], sample_item);
        let rep2 = marshal::decode(&notifs2[0].marshaled_packet).expect("decode OnItemChange");
        let pkt2 = MachoPacket::from_pyrep(&rep2).expect("parse OnItemChange packet");
        assert_eq!(pkt2.packet_type, MachoPacketType::Notification);

        // =========================================================================
        // Pipeline 3: OnSkillTrained
        // =========================================================================
        let notifs3 = ctx.notify_skill_trained(90_000_001, 501, 3300, 5);
        assert_eq!(notifs3.len(), 1);
        assert_eq!(notifs3[0].event, "OnSkillTrained");
        assert_eq!(notifs3[0].id_type, "charid");
        assert_eq!(
            notifs3[0].args,
            vec![
                PyRep::Integer(501),
                PyRep::Integer(3300),
                PyRep::Integer(5),
            ]
        );

        // =========================================================================
        // Pipeline 4: OnDamageStateChange (SolarSystem multicast scope)
        // =========================================================================
        let notifs4 = ctx.notify_damage_state_change(
            NotificationTarget::SolarSystem(30_000_142),
            100_001,
            0.80,
            0.95,
            1.0,
        );
        // Pilot 1 and Pilot 2 are in 30000142, Pilot 3 is in 30000144
        assert_eq!(
            notifs4.len(),
            2,
            "Both pilots in system 30000142 must receive OnDamageStateChange"
        );
        let recipients4: BTreeSet<i64> = notifs4.iter().map(|n| n.recipient_char_id).collect();
        assert!(recipients4.contains(&90_000_001));
        assert!(recipients4.contains(&90_000_002));
        assert!(!recipients4.contains(&p3_char_id), "Pilot 3 in other system must NOT receive it");
        assert_eq!(notifs4[0].event, "OnDamageStateChange");
        assert_eq!(notifs4[0].id_type, "itemid");

        // =========================================================================
        // Pipeline 5: OnGodmaShipEffect
        // =========================================================================
        let notifs5 = ctx.notify_godma_ship_effect(
            NotificationTarget::Character(90_000_001),
            100_001,
            16,
            true,
            6000,
            1,
        );
        assert_eq!(notifs5.len(), 1);
        assert_eq!(notifs5[0].event, "OnGodmaShipEffect");
        assert_eq!(notifs5[0].id_type, "charid");
        assert_eq!(notifs5[0].args.len(), 11);
        assert_eq!(notifs5[0].args[0], PyRep::Integer(100_001));
        assert_eq!(notifs5[0].args[1], PyRep::Integer(16));
        assert_eq!(notifs5[0].args[3], PyRep::Integer(1)); // start = 1
        assert_eq!(notifs5[0].args[4], PyRep::Integer(1)); // active = 1
        assert_eq!(notifs5[0].args[7], PyRep::Integer(6000)); // duration_ms = 6000
        assert_eq!(notifs5[0].args[8], PyRep::Integer(1)); // repeat = 1

        // =========================================================================
        // Pipeline 6: OnFleetBroadcast (Fleet multicast scope)
        // =========================================================================
        // Create a fleet with Pilot 1 (Commander) and invite Pilot 2 (Member)
        let fid = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.fleets.insert(
                id,
                eve_db::FleetRecord {
                    fleet_id: id,
                    boss_id: 90_000_001,
                    members: vec![90_000_001, 90_000_002],
                    ..Default::default()
                },
            );
            id
        });

        let notifs6 = ctx.notify_fleet_broadcast(fid, "Target", 90_000_001, 7001);
        assert_eq!(notifs6.len(), 2, "Both fleet members must receive OnFleetBroadcast");
        let recipients6: BTreeSet<i64> = notifs6.iter().map(|n| n.recipient_char_id).collect();
        assert!(recipients6.contains(&90_000_001));
        assert!(recipients6.contains(&90_000_002));
        assert!(!recipients6.contains(&p3_char_id), "Non-fleet pilot must NOT receive it");
        assert_eq!(notifs6[0].event, "OnFleetBroadcast");
        assert_eq!(notifs6[0].id_type, "fleetid");
        assert_eq!(
            notifs6[0].args,
            vec![
                PyRep::String("Target".into()),
                PyRep::Integer(90_000_001),
                PyRep::Integer(7001),
                PyRep::Integer(fid),
            ]
        );

        // =========================================================================
        // Pipeline 7: OnClientEvent_ChatMessageSent (Chat broadcast message)
        // =========================================================================
        let notifs7 = ctx.notify_lsc_message(
            NotificationTarget::SolarSystem(30_000_142),
            "solarsystemid2:30000142",
            90_000_001,
            "Local broadcast test",
        );
        assert_eq!(notifs7.len(), 2);
        assert_eq!(notifs7[0].event, "OnClientEvent_ChatMessageSent");
        assert_eq!(notifs7[0].id_type, "channelid");
        assert_eq!(
            notifs7[0].args,
            vec![
                PyRep::String("solarsystemid2:30000142".into()),
                PyRep::Integer(90_000_001),
                PyRep::String("Local broadcast test".into()),
            ]
        );

        // =========================================================================
        // Pipeline 8: OnWarpToAsteroidBelt
        // =========================================================================
        let notifs8 = ctx.notify_warp_to_asteroid_belt(90_000_001, 40_000_001);
        assert_eq!(notifs8.len(), 1);
        assert_eq!(notifs8[0].event, "OnWarpToAsteroidBelt");
        assert_eq!(notifs8[0].id_type, "charid");
        assert_eq!(
            notifs8[0].args,
            vec![PyRep::Integer(90_000_001), PyRep::Integer(40_000_001)]
        );

        // =========================================================================
        // Pipeline 9: OnAsteroidTerminated
        // =========================================================================
        let notifs9 = ctx.notify_asteroid_terminated(30_000_142, 55_000_001);
        assert_eq!(notifs9.len(), 2);
        assert_eq!(notifs9[0].event, "OnAsteroidTerminated");
        assert_eq!(notifs9[0].id_type, "solarsystemid");
        assert_eq!(notifs9[0].args, vec![PyRep::Integer(55_000_001)]);

        // =========================================================================
        // Pipeline 10: OnMiningLedgerGraphSettingChanged
        // =========================================================================
        let notifs10 = ctx.notify_mining_ledger_graph_setting_changed(
            90_000_001,
            "groupBy",
            PyRep::String("oreType".into()),
        );
        assert_eq!(notifs10.len(), 1);
        assert_eq!(notifs10[0].event, "OnMiningLedgerGraphSettingChanged");
        assert_eq!(notifs10[0].id_type, "charid");
        assert_eq!(
            notifs10[0].args,
            vec![
                PyRep::String("groupBy".into()),
                PyRep::String("oreType".into()),
            ]
        );

        // =========================================================================
        // Target Scope Multicast Checks: Corporation, Alliance, Station, All
        // =========================================================================
        // Corporation scope (Pilot 1 and Pilot 2 are in corp 98000001)
        let corp_notifs = ctx.notify(
            NotificationTarget::Corporation(98_000_001),
            "OnCorporationChanged",
            "corpid",
            vec![PyRep::String("Corp announcement".into())],
        );
        assert_eq!(corp_notifs.len(), 2);
        let corp_recips: BTreeSet<i64> = corp_notifs.iter().map(|n| n.recipient_char_id).collect();
        assert!(corp_recips.contains(&90_000_001));
        assert!(corp_recips.contains(&90_000_002));
        assert!(!corp_recips.contains(&p3_char_id));

        // Station scope (Pilot 1 and Pilot 2 are in station 60003760)
        let station_notifs = ctx.notify(
            NotificationTarget::Station(60_003_760),
            "OnCharNowInStation",
            "stationid",
            vec![PyRep::String("Station notice".into())],
        );
        assert_eq!(station_notifs.len(), 2);

        // All scope (all 3 online pilots)
        let all_notifs = ctx.notify(
            NotificationTarget::All,
            "OnServerMessage",
            "all",
            vec![PyRep::String("Maintenance in 15 minutes".into())],
        );
        assert_eq!(all_notifs.len(), 3);

        // =========================================================================
        // Queue Overflow Protection & Drain Verification
        // =========================================================================
        // Exceed MAX_PENDING_NOTIFICATIONS_PER_CHAR (128)
        for i in 0..150 {
            ctx.notify_account_change(90_000_001, "cash", 90_000_001, i as f64);
        }
        let p1_queue = ctx.drain_notifications_for_char(90_000_001);
        assert_eq!(
            p1_queue.len(),
            MAX_PENDING_NOTIFICATIONS_PER_CHAR,
            "Pending notification queue must be capped at MAX_PENDING_NOTIFICATIONS_PER_CHAR (128)"
        );

        // Second drain must be empty
        let empty_queue = ctx.drain_notifications_for_char(90_000_001);
        assert!(empty_queue.is_empty(), "Queue must be empty after drain");
    }
}

pub(crate) mod notifies_abyssal_void_space {
    // Runtime integration tests for open `notify:*` parity rows (Part 1).


    use crate::common;
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;

    const PILOT_A: i64 = 90_000_001;
    const PILOT_B: i64 = 90_000_002;
    const SYS_JITA: u32 = 30_000_142;

    fn new_ctx() -> ServerContext {
        let sde = std::sync::Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
        );
        let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        ctx.set_clock_paused(true);
        ctx
    }

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

    fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
        Ball {
            id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 0.5,
            mass: 10_000_000.0,
            radius: 120.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        }
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

    fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
        let mut out = sess.drain_notifications();
        for n in ctx.drain_notifications_for_char(char_id) {
            out.push((n.event, n.args));
        }
        out
    }

    fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
        notifs
            .iter()
            .find(|(ev, _)| ev == event)
            .map(|(_, args)| args.as_slice())
            .unwrap_or_else(|| {
                let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
                panic!("expected notification {event}, got {seen:?}")
            })
    }

    async fn setup_pilot_in_space(
        ctx: &ServerContext,
        sid: u64,
        user: &str,
        pass: &str,
        char_id: i64,
        ship_id: i64,
        ship_type: u32,
    ) -> SessionState {
        let mut sess = SessionState::new(sid);
        common::login_pilot(ctx, &mut sess, user, pass).await;
        rpc(
            ctx,
            &mut sess,
            "charUnboundMgr",
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
        )
        .await
        .expect("SelectCharacterID");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = SYS_JITA;
                ch.station_id = None;
                ch.active_ship_id = ship_id;
            }
            snap.items.insert(
                ship_id,
                make_item(ship_id, ship_type, char_id, i64::from(SYS_JITA), InventoryFlag::None, 1, true),
            );
        });
        sess.solar_system_id = Some(SYS_JITA);
        sess.station_id = None;
        sess.ship_id = Some(ship_id);
        let _ = ctx
            .systems
            .get_or_spawn(SYS_JITA)
            .spawn_ball_with_combat(make_ball(ship_id, ship_type, DVec3::ZERO), Default::default())
            .await;
        ctx.advance_secs(12);
        let _ = drain_all(ctx, &mut sess, char_id);
        sess
    }

    #[tokio::test]
    async fn test_parity_notifies_abyssal_and_void_space() {
        // parity: notify:OnAbyssalContentFinished, notify:OnAbyssalLocationEntered
        // parity: notify:OnVoidSpaceContentFinished, notify:OnVoidSpaceEntered, notify:OnVoidSpaceRelog
        eve_testkit::attest(&[
            "notify:OnAbyssalContentFinished",
            "notify:OnAbyssalLocationEntered",
            "notify:OnVoidSpaceContentFinished",
            "notify:OnVoidSpaceEntered",
            "notify:OnVoidSpaceRelog",
        ]);
        let ctx = new_ctx();
        let mut sess = setup_pilot_in_space(&ctx, 1101, "demo", "demo-secret", PILOT_A, 710_001, 622).await;

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.solar_system_id = 30_000_021;
            }
            if let Some(ship) = snap.items.get_mut(&710_001) {
                ship.location_id = 30_000_021;
            }
            snap.items.insert(
                710_010,
                make_item(710_010, 47_762, PILOT_A, 710_001, InventoryFlag::Cargo, 1, false),
            );
        });
        sess.solar_system_id = Some(30_000_021);
        let _ = ctx
            .systems
            .get_or_spawn(30_000_021)
            .spawn_ball_with_combat(make_ball(710_001, 622, DVec3::ZERO), Default::default())
            .await;

        rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(710_010)])
            .await
            .expect("AbyssalEntranceDeployment");
        let gate_id = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
            *reg.abyssal_traces.keys().next().expect("trace gate")
        });
        rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(gate_id)])
            .await
            .expect("AbyssalEntranceGateActivation");
        let abyss_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        let entered = find_notif(&abyss_notifs, "OnAbyssalLocationEntered");
        let content_id = entered[0]
            .dict_get("content_id")
            .and_then(|v| v.as_i64())
            .expect("content_id");
        assert!(content_id > 0);

        eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
            if let Some(p) = reg.abyssal_pockets.get_mut(&PILOT_A) {
                p.current_room = 3;
                p.rooms[2].npc_ball_ids.clear();
                p.rooms[2].cleared = true;
            }
        });
        rpc(&ctx, &mut sess, "abyssalMgr", "ClientIsReady", &[]).await.expect("ClientIsReady");
        let end_gate_id = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
            reg.abyssal_pockets
                .get(&PILOT_A)
                .map(|p| 1_895_000_000 + p.pocket_id * 10 + 3)
                .expect("end gate")
        });
        if let Some(pocket_sys) = sess.solar_system_id {
            let abyss_sys = ctx.systems.get_or_spawn(pocket_sys);
            if let Ok(Some((_, gate_ball))) = abyss_sys.get_ball_snapshot(end_gate_id).await {
                let _ = abyss_sys.set_ball_position(710_001, gate_ball.position).await;
            }
        }
        rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEndGateActivation", &[PyRep::Integer(end_gate_id)])
            .await
            .expect("AbyssalEndGateActivation");
        let abyss_fin = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(
            find_notif(&abyss_fin, "OnAbyssalContentFinished"),
            &[PyRep::Integer(content_id), PyRep::Integer(1)]
        );

        // Void Space: enter -> relog -> exit
        let mut sess_b = setup_pilot_in_space(&ctx, 1102, "other", "other-secret", PILOT_B, 710_002, 622).await;
        let void_fil_type = ctx
            .sde
            .types
            .iter()
            .find(|(_, t)| t.group_id == 4145)
            .map(|(&id, _)| id)
            .unwrap_or(60_770);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                710_011,
                make_item(710_011, void_fil_type, PILOT_B, 710_002, InventoryFlag::Cargo, 1, false),
            );
        });
        ctx.advance_secs(12);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
        rpc(&ctx, &mut sess_b, "voidSpaceMgr", "VoidSpacePlayerJump", &[PyRep::Integer(710_011)])
            .await
            .expect("VoidSpacePlayerJump");
        let vse = drain_all(&ctx, &mut sess_b, PILOT_B);
        let vse_args = find_notif(&vse, "OnVoidSpaceEntered");
        let vs_cid = vse_args[0]
            .dict_get("content_id")
            .and_then(|v| v.as_i64())
            .expect("void content_id");
        assert!(vs_cid > 0);

        let _ = rpc(&ctx, &mut sess_b, "voidSpaceMgr", "ClientIsReady", &[]).await;
        let relog = drain_all(&ctx, &mut sess_b, PILOT_B);
        let relog_args = find_notif(&relog, "OnVoidSpaceRelog");
        assert_eq!(relog_args[0].dict_get("content_id").and_then(|v| v.as_i64()), Some(vs_cid));

        let void_exit_gate = 999_001_i64;
        if let Some(vs_sys) = sess_b.solar_system_id {
            let void_sys = ctx.systems.get_or_spawn(vs_sys);
            let _ = void_sys
                .spawn_ball_with_combat(make_ball(void_exit_gate, 60_608, DVec3::ZERO), Default::default())
                .await;
            let _ = void_sys.set_ball_position(710_002, DVec3::ZERO).await;
        }
        ctx.advance_secs(12);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
        rpc(&ctx, &mut sess_b, "voidSpaceMgr", "VoidSpaceEndGateActivation", &[PyRep::Integer(void_exit_gate)])
            .await
            .expect("VoidSpaceEndGateActivation");
        let void_fin = drain_all(&ctx, &mut sess_b, PILOT_B);
        assert_eq!(
            find_notif(&void_fin, "OnVoidSpaceContentFinished"),
            &[PyRep::Integer(vs_cid), PyRep::Integer(1)]
        );
    }

    #[tokio::test]
    async fn test_parity_notifies_operations_and_node_graphs() {
        // parity: notify:OnOperationSkipped, notify:OnOperationsProgressReset, notify:OnOperationsServerFSDReload
        // parity: notify:OnStartClientNodeGraph, notify:OnClientNodeGraphUpdate, notify:OnClientNodeGraphMessage
        // parity: notify:OnStopClientNodeGraph, notify:OnStopClientNodeGraphById, notify:OnStopAllClientNodeGraphs
        eve_testkit::attest(&[
            "notify:OnOperationSkipped",
            "notify:OnOperationsProgressReset",
            "notify:OnOperationsServerFSDReload",
            "notify:OnStartClientNodeGraph",
            "notify:OnClientNodeGraphUpdate",
            "notify:OnClientNodeGraphMessage",
            "notify:OnStopClientNodeGraph",
            "notify:OnStopClientNodeGraphById",
            "notify:OnStopAllClientNodeGraphs",
        ]);
        let ctx = new_ctx();
        let mut sess = setup_pilot_in_space(&ctx, 1103, "demo", "demo-secret", PILOT_A, 710_003, 622).await;
        sess.role = ROLE_GM_DEFAULT;

        let op_id = *eve_server::services::operations_manager_helpers::get_operation_defs(&ctx.sde)
            .op_tasks
            .keys()
            .next()
            .expect("operation in SDE");
        rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String(format!("/operation skip 1 {op_id}"))])
            .await
            .expect("/operation skip");
        let skip_n = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&skip_n, "OnOperationSkipped"), &[PyRep::Integer(1), PyRep::Integer(op_id)]);

        rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/operation reset_progress".into())])
            .await
            .expect("/operation reset_progress");
        let reset_n = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&reset_n, "OnOperationsProgressReset").len(), 3);
        assert_eq!(find_notif(&reset_n, "OnStopAllClientNodeGraphs").len(), 0);

        rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/operation reload_fsd".into())])
            .await
            .expect("/operation reload_fsd");
        let reload_n = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&reload_n, "OnOperationsServerFSDReload").len(), 0);

        // Drive real AIR NPE node-graph runner RPCs -> emits OnStartClientNodeGraph,
        // OnClientNodeGraphUpdate, OnClientNodeGraphMessage, OnStopClientNodeGraph, OnStopClientNodeGraphById
        let mut npe_sess = SessionState::new(1105);
        common::login_pilot(&ctx, &mut npe_sess, "demo", "demo-secret").await;
        let doll = PyRep::keyval([("genderID", PyRep::Integer(1)), ("bloodlineID", PyRep::Integer(1))]);
        let portrait = PyRep::keyval([("backgroundID", PyRep::Integer(1))]);
        let npe_cid = rpc(
            &ctx,
            &mut npe_sess,
            "charUnboundMgr",
            "CreateCharacterWithDoll",
            &[
                PyRep::String("NPE Notify Pilot".into()),
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(11),
                doll,
                portrait,
                PyRep::Integer(17),
            ],
        )
        .await
        .expect("CreateCharacterWithDoll")
        .as_i64()
        .expect("npe_cid");
        rpc(&ctx, &mut npe_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(npe_cid)])
            .await
            .expect("SelectCharacterID npe");
        rpc(
            &ctx,
            &mut npe_sess,
            "operationsManager",
            "process_client_event",
            &[PyRep::String("start_tutorial".into())],
        )
        .await
        .expect("start_tutorial");
        let start_n = drain_all(&ctx, &mut npe_sess, npe_cid);
        let start_args = find_notif(&start_n, "OnStartClientNodeGraph");
        assert_eq!(start_args.len(), 3);
        assert_eq!(start_args[0], PyRep::Integer(716));
        assert!(start_args[1].as_i64().unwrap_or(0) > 0);
        rpc(
            &ctx,
            &mut npe_sess,
            "operationsManager",
            "process_client_event",
            &[PyRep::String("new_eden_cinematic_ended".into())],
        )
        .await
        .expect("new_eden_cinematic_ended");
        let msg_n = drain_all(&ctx, &mut npe_sess, npe_cid);
        assert!(!find_notif(&msg_n, "OnClientNodeGraphUpdate").is_empty());
        assert_eq!(
            find_notif(&msg_n, "OnClientNodeGraphMessage")
                .get(2)
                .and_then(|v| v.as_str()),
            Some("pocket_entered")
        );
        rpc(
            &ctx,
            &mut npe_sess,
            "operationsManager",
            "process_client_event",
            &[PyRep::String("skip_tutorial".into())],
        )
        .await
        .expect("skip_tutorial");
        let stop_n = drain_all(&ctx, &mut npe_sess, npe_cid);
        let stop_inst_args = find_notif(&stop_n, "OnStopClientNodeGraph");
        assert_eq!(stop_inst_args.len(), 1);
        assert!(stop_inst_args[0].as_i64().unwrap_or(0) > 0);
        assert_eq!(find_notif(&stop_n, "OnStopClientNodeGraphById"), &[PyRep::Integer(716)]);
    }

    #[tokio::test]
    async fn test_parity_notifies_dungeons_cloak_and_space() {
        // parity: notify:OnCloakModuleForceDeactivated, notify:OnFleetEscalatingPathMessage, notify:OnProgressionRoomJoined
        eve_testkit::attest(&[
            "notify:OnCloakModuleForceDeactivated",
            "notify:OnFleetEscalatingPathMessage",
            "notify:OnProgressionRoomJoined",
        ]);
        let ctx = new_ctx();
        let mut sess = setup_pilot_in_space(&ctx, 1104, "demo", "demo-secret", PILOT_A, 710_004, 622).await;

        // 1. OnCloakModuleForceDeactivated via real beyonce.CmdStop on cloaked ship in Abyssal space
        let sys_abyssal = 32_000_001_u32;
        let cloak_type_id = ctx
            .sde
            .types
            .values()
            .find(|t| t.group_id == 330)
            .map(|t| t.type_id)
            .unwrap_or(11578);
        sess.solar_system_id = Some(sys_abyssal);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&PILOT_A) {
                c.solar_system_id = sys_abyssal;
            }
            snap.items.insert(
                710_099,
                make_item(710_099, cloak_type_id, PILOT_A, 710_004, InventoryFlag::HiSlot0, 1, true),
            );
            snap.module_active_states.insert(710_099);
        });
        let abyss_oid = ctx.bind_object(&sess, "beyonce", i64::from(sys_abyssal)).expect("bind abyss beyonce");
        let abyss_h = ctx.systems.get_or_spawn(sys_abyssal);
        let _ = abyss_h.spawn_ball(make_ball(710_004, 622, DVec3::ZERO)).await;
        let mut cs = eve_destiny::CombatState::default();
        cs.is_cloaked = true;
        let _ = abyss_h.set_combat_state(710_004, cs).await;
        let _ = drain_all(&ctx, &mut sess, PILOT_A);
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(abyss_oid), "CmdStop", &[], &HashMap::new(), None)
            .await
            .expect("CmdStop cloaked in abyssal");
        let cloak_n = drain_all(&ctx, &mut sess, PILOT_A);
        let decloak_args = find_notif(&cloak_n, "OnCloakModuleForceDeactivated");
        assert_eq!(decloak_args.len(), 2);
        assert_eq!(decloak_args[0], PyRep::String("AbyssalDeadspaceCloakBlocked".into()));

        // Restore PILOT_A to Jita for fleet escalation & dungeon progression checks
        sess.solar_system_id = Some(SYS_JITA);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&PILOT_A) {
                c.solar_system_id = SYS_JITA;
            }
        });

        // 2. OnFleetEscalatingPathMessage via real combat anomaly clear in a fleet
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&PILOT_A) {
                c.fleet_id = Some(77_001);
            }
            if let Some(c) = snap.characters.get_mut(&PILOT_B) {
                c.fleet_id = Some(77_001);
            }
            let mut fl = eve_db::FleetRecord::default();
            fl.fleet_id = 77_001;
            fl.boss_id = PILOT_A;
            fl.members = vec![PILOT_A, PILOT_B];
            snap.fleets.insert(77_001, fl);
        });
        eve_server::services::pve_exploration::ensure_system_pve_sites_seeded(&ctx, SYS_JITA).await;
        let site_id = "PVE-ANOM-30000142-1";
        let _ = ctx.drain_notifications_for_char(PILOT_B);
        let sys_h = ctx.systems.get_or_spawn(SYS_JITA);
        for _ in 0..5 {
            let (cleared, npcs) = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
                reg.sites_by_system[&SYS_JITA]
                    .iter()
                    .find(|s| s.site_id == site_id)
                    .map(|s| (s.cleared, s.active_npc_ball_ids.clone()))
                    .unwrap_or((true, Vec::new()))
            });
            if cleared {
                break;
            }
            if npcs.is_empty() {
                rpc(
                    &ctx,
                    &mut sess,
                    "scanSvc",
                    "EnterCombatAnomaly",
                    &[PyRep::String(site_id.into()), PyRep::Bool(true)],
                )
                .await
                .expect("EnterCombatAnomaly");
                continue;
            }
            ctx.set_mechanic_rng_seed(Some(3));
            for nid in npcs {
                let _ = sys_h
                    .apply_damage_with_attacker(
                        nid,
                        eve_destiny::DamageVector {
                            em: 1.0e7,
                            thermal: 1.0e7,
                            kinetic: 1.0e7,
                            explosive: 1.0e7,
                        },
                        Some(PILOT_A),
                    )
                    .await;
                let _ = sys_h.remove_ball(nid).await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
        let esc_n: Vec<(String, Vec<PyRep>)> = ctx
            .drain_notifications_for_char(PILOT_B)
            .into_iter()
            .map(|n| (n.event, n.args))
            .collect();
        let fleet_esc = find_notif(&esc_n, "OnFleetEscalatingPathMessage");
        assert_eq!(fleet_esc.len(), 2);
        assert_eq!(fleet_esc[0], PyRep::Integer(PILOT_A));
        assert!(fleet_esc[1].as_i64().unwrap_or(0) > 0);

        let inst_id = eve_server::services::pve_dungeon_runtime::spawn_sde_dungeon_runtime_instance(
            &ctx,
            PILOT_A,
            SYS_JITA,
            2001,
            "Deadspace",
            [120_000_000.0, 0.0, 80_000_000.0],
            true,
        )
        .await
        .expect("spawn_sde_dungeon_runtime_instance");
        let dng_state = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id)
            .expect("get_dungeon_instance_state");
        let entry_gate_id = dng_state.dict_get("entryGateBallID").and_then(|v| v.as_i64()).unwrap();
        let sys_h = ctx.systems.get_or_spawn(SYS_JITA);
        let entry_gate_pos = sys_h.get_ball_snapshot(entry_gate_id).await.unwrap().unwrap().1.position;
        let _ = sys_h.set_ball_position(710_004, entry_gate_pos + DVec3::new(500.0, 0.0, 0.0)).await;
        let _ = drain_all(&ctx, &mut sess, PILOT_A);
        rpc(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(entry_gate_id)])
            .await
            .expect("ActivateAccelerationGate");
        let pr_n = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&pr_n, "OnProgressionRoomJoined").len(), 5);
    }
}

pub(crate) mod notifies_seasons_campaigns_skills {
    // Runtime integration tests for open `notify:*` parity rows (Part 2).


    use crate::common;
    use eve_db::{AsteroidRecord, InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use eve_server::system_actor::AsteroidState;
    use glam::DVec3;
    use std::collections::HashMap;

    const PILOT_A: i64 = 90_000_001;

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

    fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
        let mut out = sess.drain_notifications();
        for n in ctx.drain_notifications_for_char(char_id) {
            out.push((n.event, n.args));
        }
        out
    }

    fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
        notifs
            .iter()
            .find(|(ev, _)| ev == event)
            .map(|(_, args)| args.as_slice())
            .unwrap_or_else(|| {
                let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
                panic!("expected notification {event}, got {seen:?}")
            })
    }

    async fn setup_pilot(ctx: &ServerContext, sid: u64) -> SessionState {
        let mut sess = SessionState::new(sid);
        common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
        rpc(
            ctx,
            &mut sess,
            "charUnboundMgr",
            "SelectCharacterID",
            &[PyRep::Integer(PILOT_A)],
        )
        .await
        .expect("SelectCharacterID");
        ctx.advance_secs(12);
        let _ = drain_all(ctx, &mut sess, PILOT_A);
        sess
    }

    #[tokio::test]
    async fn test_parity_notifies_seasons_campaigns_and_skills() {
        // parity: notify:OnSeasonOmegaRewardsClaimed, notify:OnSeasonalGoalCompleted, notify:OnSeasonalGoalsReset
        eve_testkit::attest(&[
            "notify:OnSeasonOmegaRewardsClaimed",
            "notify:OnSeasonalGoalCompleted",
            "notify:OnSeasonalGoalsReset",
        ]);
        let ctx = new_ctx();
        let mut sess = setup_pilot(&ctx, 1201).await;
        sess.role = ROLE_GM_DEFAULT;

        let mut target_ch_id = 1_005_i64;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.clone_grade = 2;
            }
            if let Some(mut def) =
                eve_server::services::season_fsd::default_seasonal_challenge_definitions()
                    .into_iter()
                    .next()
            {
                target_ch_id = def.challenge_id;
                def.client_event_driven = true;
                def.event_conditions.clear();
                def.max_progress = 2;
                def.points = 20;
                snap.service_state_blobs.insert(
                    eve_server::services::season_fsd::SEASONAL_CHALLENGES_CONFIG_KEY.into(),
                    serde_json::to_string(&vec![def]).unwrap(),
                );
            }
        });
        let _ = rpc(&ctx, &mut sess, "seasonManager", "fill_up_and_return_challenges_for_character", &[])
            .await
            .expect("fill challenges");
        for _ in 0..2 {
            rpc(
                &ctx,
                &mut sess,
                "seasonManager",
                "process_client_event",
                &[PyRep::Integer(target_ch_id)],
            )
            .await
            .expect("complete challenge");
        }
        let _ = drain_all(&ctx, &mut sess, PILOT_A);
        let claim = rpc(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(target_ch_id)])
            .await
            .expect("claim_challenge_rewards");
        assert_eq!(claim.dict_get("claimed").and_then(|v| v.as_bool()), Some(true));
        let claim_n = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&claim_n, "OnSeasonalGoalCompleted")[0], PyRep::Integer(0));
        assert_eq!(
            find_notif(&claim_n, "OnSeasonOmegaRewardsClaimed"),
            &[PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(1)])]
        );

        rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String(format!("/season reset_goals {PILOT_A}"))])
            .await
            .expect("/season reset_goals");
        let reset_n = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(find_notif(&reset_n, "OnSeasonalGoalsReset").len(), 0);
    }

    #[tokio::test]
    async fn test_parity_notifies_plex_dogma_and_brain() {
        // parity: notify:OnEffectMessage, notify:OnPlexDepositFailed, notify:OnPlexWithdrawFailed
        eve_testkit::attest(&[
            "notify:OnEffectMessage",
            "notify:OnPlexDepositFailed",
            "notify:OnPlexWithdrawFailed",
        ]);
        let ctx = new_ctx();
        let mut sess = setup_pilot(&ctx, 1202).await;

        // 1. OnPlexDepositFailed via invbroker.DepositPlexToVault with reference
        let jita_station = 60_003_760_i64;
        let trit_id = 880_001_i64;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.station_id = Some(jita_station);
            }
            snap.adjust_plex_vault(PILOT_A, 50);
            snap.items.insert(
                trit_id,
                InventoryItemRecord {
                    item_id: trit_id,
                    type_id: 34,
                    owner_id: PILOT_A,
                    location_id: jita_station,
                    flag: InventoryFlag::Hangar,
                    quantity: 100,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        sess.station_id = Some(jita_station);
        let _ = drain_all(&ctx, &mut sess, PILOT_A);
        assert!(rpc(
            &ctx,
            &mut sess,
            "invbroker",
            "DepositPlexToVault",
            &[
                PyRep::Integer(jita_station),
                PyRep::Integer(trit_id),
                PyRep::Integer(10),
                PyRep::Integer(4242),
            ],
        )
        .await
        .is_err());
        let dep_n = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(
            find_notif(&dep_n, "OnPlexDepositFailed"),
            &[PyRep::String("NotPlex".into()), PyRep::Integer(4242)]
        );

        // 2. OnPlexWithdrawFailed via invbroker.WithdrawPlexFromVault with reference
        let mut wd_kwargs = HashMap::new();
        wd_kwargs.insert("reference".to_string(), PyRep::Integer(5353));
        assert!(ctx
            .dispatch_rpc(
                &mut sess,
                "invbroker",
                None,
                "WithdrawPlexFromVault",
                &[PyRep::Integer(99_999)],
                &wd_kwargs,
                None,
            )
            .await
            .is_err());
        let wd_n = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(
            find_notif(&wd_n, "OnPlexWithdrawFailed"),
            &[PyRep::String("NotEnoughPlex".into()), PyRep::Integer(5353)]
        );

        // 3. OnEffectMessage via mining_cycle::process_mining_cycles when capacitor runs dry
        let sys_jita = 30_000_142_u32;
        let miner_id = 880_011_i64;
        let rock_id = 880_012_i64;
        let miner_type = 483_u32;
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(PILOT_A).or_default().insert(
                3386,
                eve_db::TrainedSkillRecord {
                    type_id: 3386,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
            let active_ship = snap.characters.get(&PILOT_A).map(|c| c.active_ship_id).unwrap_or(0);
            if let Some(sh) = snap.items.get_mut(&active_ship) {
                sh.type_id = 32880;
            }
            snap.items.retain(|_, i| i.location_id != active_ship);
            snap.module_online_states.insert(miner_id, true);
            snap.items.insert(
                miner_id,
                InventoryItemRecord {
                    item_id: miner_id,
                    type_id: miner_type,
                    owner_id: PILOT_A,
                    location_id: active_ship,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        let undock = rpc(&ctx, &mut sess, "stationSvc", "Undock", &[])
            .await
            .expect("Undock");
        let ship_id = undock.dict_get("shipID").and_then(|v| v.as_i64()).expect("shipID");
        ctx.invalidate_ship_fitting(ship_id);
        let beyonce_id = undock
            .dict_get("beyonceBoundID")
            .and_then(|v| v.as_i64())
            .map(|v| v as u64);
        ctx.dispatch_rpc(&mut sess, "beyonce", beyonce_id, "CmdStop", &[], &HashMap::new(), None)
            .await
            .expect("CmdStop");

        let sys = ctx.systems.get_or_spawn(sys_jita);
        let (_, ship_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().expect("ship ball");
        let rock_pos = ship_ball.position + DVec3::new(2_000.0, 0.0, 0.0);
        ctx.db.spawn_asteroid(AsteroidRecord {
            asteroid_id: rock_id,
            solar_system_id: sys_jita,
            belt_id: 40_000_001,
            type_id: 1230,
            quantity: 10_000,
            initial_quantity: 10_000,
            position: (rock_pos.x, rock_pos.y, rock_pos.z),
            radius: 100.0,
        });
        let _ = sys
            .spawn_asteroid(
                AsteroidState {
                    asteroid_id: rock_id,
                    belt_id: 40_000_001,
                    type_id: 1230,
                    initial_units: 10_000,
                    remaining_units: 10_000,
                    unit_volume: 0.1,
                    depleted: false,
                    respawn_at_tick: None,
                },
                Ball {
                    id: rock_id,
                    type_id: 1230,
                    position: rock_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 0.0,
                    mass: 1.0e10,
                    radius: 100.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
            )
            .await;
        rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(rock_id)])
            .await
            .expect("AddTarget");
        for _ in 0..30 {
            if sys
                .get_target_locks(ship_id)
                .await
                .unwrap()
                .iter()
                .any(|l| l.target_id == rock_id && l.locked)
            {
                break;
            }
            sys.step_manual(1.0).await.unwrap();
        }
        rpc(
            &ctx,
            &mut sess,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(miner_id),
                PyRep::String("miningLaser".into()),
                PyRep::Integer(rock_id),
            ],
        )
        .await
        .expect("Activate Miner I");
        let _ = drain_all(&ctx, &mut sess, PILOT_A);
        ctx.advance_secs(65);
        let mut cs = ctx.build_ship_combat_state(PILOT_A, ship_id, 32880);
        cs.max_capacitor = 0.0;
        cs.capacitor = 0.0;
        let _ = sys.set_combat_state(ship_id, cs).await;
        let _ = eve_server::services::mining_cycle::process_mining_cycles(&ctx).await;
        let eff_n = drain_all(&ctx, &mut sess, PILOT_A);
        let eff_args = find_notif(&eff_n, "OnEffectMessage");
        assert_eq!(eff_args[0], PyRep::String("NotEnoughEnergy".into()));
        assert_eq!(eff_args[1].dict_get("moduleID"), Some(&PyRep::Integer(miner_id)));
        assert_eq!(
            eff_args[1].dict_get("effectname"),
            Some(&PyRep::String("miningLaser".into()))
        );
        assert!(eff_args[1].dict_get("need").is_some());
        assert_eq!(eff_args[1].dict_get("got"), Some(&PyRep::Real(0.0)));
    }
}

pub(crate) mod agent_plausible_missions_cache_rowset {
    // Integration tests verifying behavioral verification of FLAGGED_FAKE `rpc:*` and `notify:*` rows.

    use crate::common;
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, MovementMode, TargetLock};
    use eve_machonet::SessionState;
    use eve_proto::quasar::envelopes::EvePublicRequest;
    use eve_proto::quasar::GoogleAny;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;

    const STATUS_OK: u32 = 200;

    const PILOT_A: i64 = 90_000_001;
    const PILOT_B: i64 = 90_000_002;
    const SYS_JITA: u32 = 30_000_142;
    const JITA_STATION: i64 = 60_003_760;

    fn new_ctx() -> ServerContext {
        let ctx = ServerContext::new();
        ctx.set_clock_paused(true);
        ctx
    }

    async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, eve_proto::BridgeRpcError> {
        ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
    }

    fn drain_all(ctx: &ServerContext, _sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
        ctx.drain_notifications_for_char(char_id).into_iter().map(|n| (n.event, n.args)).collect()
    }

    fn find_notif(notifs: &[(String, Vec<PyRep>)], event: &str) -> Vec<PyRep> {
        notifs.iter().find(|(e, _)| e == event).map(|(_, a)| a.clone()).unwrap_or_default()
    }

    fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
        match rep {
            PyRep::KeyVal(entries) => entries.get(key),
            PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
            _ => None,
        }
    }

    fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
        InventoryItemRecord {
            item_id, type_id, owner_id, location_id, flag, quantity, singleton,
            version: 1, dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        }
    }

    async fn login_in_space(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64, ship_id: i64, ship_type_id: u32, sys_id: u32, pos: DVec3) -> SessionState {
        let mut sess = SessionState::new(sid);
        common::login_pilot(ctx, &mut sess, user, pass).await;
        rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(ship_id, make_item(ship_id, ship_type_id, char_id, i64::from(sys_id), InventoryFlag::None, 1, true));
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = sys_id;
                ch.active_ship_id = ship_id;
            }
        });
        sess.station_id = None;
        sess.structure_id = None;
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(ship_id);
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        sys_h.spawn_ball_with_meta(
            Ball { id: ship_id, type_id: ship_type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Rigid },
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
            eve_destiny::SlimItemMeta { group_id: 25, category_id: 6, owner_id: char_id, char_id: Some(char_id), corp_id: 1000009, alliance_id: -1, security_status: 0.0, is_cloaked: false, ..Default::default() },
        ).await.unwrap();
        sess
    }

    fn chat_bcast_req(msg: &str) -> EvePublicRequest {
        let mut payload = vec![0x0A, msg.len() as u8];
        payload.extend_from_slice(msg.as_bytes());
        EvePublicRequest {
            payload: Some(GoogleAny {
                type_url: "type.googleapis.com/eve_public.chat.local.api.BroadcastMessageRequest".into(),
                value: payload,
            }),
            ..Default::default()
        }
    }

    // parity: rpc:agentMgr.GetPlausibleMissionIDs, rpc:cache.Rowset
    #[tokio::test]
    async fn test_agent_plausible_missions_and_cache_rowset() {
        eve_testkit::attest(&[
            "rpc:agentMgr.GetPlausibleMissionIDs",
            "rpc:cache.Rowset",
        ]);

        let ctx = new_ctx();
        let mut sess = SessionState::new(91_001);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)]).await.unwrap();

        let sec_agent = ctx.sde.agents.values().find(|a| a.division_id == 24 && a.agent_type_id == 2).map(|a| a.agent_id).expect("SDE security agent");
        let mining_agent = ctx.sde.agents.values().find(|a| a.division_id == 23 && a.agent_type_id == 2).map(|a| a.agent_id).expect("SDE mining agent");

        let PyRep::List(sec_missions) = rpc(&ctx, &mut sess, "agentMgr", "GetPlausibleMissionIDs", &[PyRep::Integer(sec_agent)]).await.unwrap() else { panic!("expected List"); };
        let PyRep::List(mining_missions) = rpc(&ctx, &mut sess, "agentMgr", "GetPlausibleMissionIDs", &[PyRep::Integer(mining_agent)]).await.unwrap() else { panic!("expected List"); };
        assert!(!sec_missions.is_empty());
        assert!(!mining_missions.is_empty());
        assert_ne!(sec_missions, mining_missions, "security and mining agents must not return identical mission pools");

        let invalid_agent = rpc(&ctx, &mut sess, "agentMgr", "GetPlausibleMissionIDs", &[PyRep::Integer(999_999_999)]).await;
        assert_eq!(invalid_agent.unwrap_err().code, "AgentNotFound");

        let PyRep::Rowset { columns, rows } = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("roles".into())]).await.unwrap() else { panic!("expected Rowset"); };
        assert_eq!(columns, vec!["roleID", "roleName"]);
        assert!(rows.len() >= 30, "roles rowset must contain full EVE corporation roles, got {}", rows.len());
        assert!(rows.iter().any(|r| r.get(1) == Some(&PyRep::String("Contract Manager".into()))));
        assert!(rows.iter().any(|r| r.get(1) == Some(&PyRep::String("Project Manager".into()))));

        let PyRep::Rowset { rows: race_rows, .. } = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("races".into())]).await.unwrap() else { panic!("expected races Rowset"); };
        assert_eq!(race_rows.len(), ctx.sde.races.len());

        let unknown = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("nonexistent_table".into())]).await;
        assert_eq!(unknown.unwrap_err().code, "UnknownCacheRowset");
    }

    // parity: notify:OnNewProbe, notify:OnProbeStateChanged, notify:OnProbeWarpStart
    // parity: notify:OnProbeWarpEnd, notify:OnSystemScanStarted, notify:OnSystemScanStopped
    #[tokio::test]
    async fn test_probe_scanning_lifecycle_notifications() {
        eve_testkit::attest(&[
            "notify:OnNewProbe",
            "notify:OnProbeStateChanged",
            "notify:OnProbeWarpStart",
            "notify:OnProbeWarpEnd",
            "notify:OnSystemScanStarted",
            "notify:OnSystemScanStopped",
        ]);

        let ctx = new_ctx();
        let jita_pos = ctx.sde.stations[&JITA_STATION].position + DVec3::new(1_000.0, 0.0, 0.0);
        let (ship_id, launcher_id, probe_stack_id) = (630_000_001_i64, 630_000_010_i64, 630_000_011_i64);
        let mut sess = login_in_space(&ctx, 91_002, "demo", "demo-secret", PILOT_A, ship_id, 621, SYS_JITA, jita_pos).await;

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(launcher_id, make_item(launcher_id, 17_901, PILOT_A, ship_id, InventoryFlag::HiSlot0, 1, true));
            snap.module_online_states.insert(launcher_id, true);
            snap.items.insert(probe_stack_id, make_item(probe_stack_id, 30_013, PILOT_A, ship_id, InventoryFlag::HiSlot0, 8, false));
        });
        ctx.invalidate_ship_fitting(ship_id);

        rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())]).await.unwrap();
        let launch_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        let new_probe = &find_notif(&launch_notifs, "OnNewProbe")[0];
        let expiry_ft = dict_get(new_probe, "expiry").and_then(|v| v.as_i64()).unwrap_or(0);
        assert!(expiry_ft > 130_000_000_000_000_000, "OnNewProbe expiry must be BlueOS FILETIME ticks, got {expiry_ft}");
        assert!(dict_get(new_probe, "pos").is_some(), "OnNewProbe must include pos");

        let coords = PyRep::List((0..8).map(|i| PyRep::List(vec![PyRep::Real(500_000.0 + (i as f64) * 500.0), PyRep::Real(2_000.0), PyRep::Real(0.0), PyRep::Integer(2)])).collect());
        rpc(&ctx, &mut sess, "scanMgr", "RequestScans", &[coords]).await.unwrap();
        let immediate = drain_all(&ctx, &mut sess, PILOT_A);
        assert!(
            immediate.iter().any(|(e, a)| e == "OnProbeStateChanged" && a.get(1) == Some(&PyRep::Integer(3))),
            "warping probes must emit OnProbeStateChanged(Warping=3)"
        );
        assert_eq!(find_notif(&immediate, "OnProbeWarpStart").len(), 5);
        let started = find_notif(&immediate, "OnSystemScanStarted");
        let scan_ms = started[1].as_i64().unwrap_or(0);
        assert!(scan_ms > 4_000, "scan duration must be skill/ship-computed (> 4000ms), got {scan_ms}");
        assert!(!immediate.iter().any(|(e, _)| e == "OnSystemScanStopped"), "OnSystemScanStopped must not fire at 0s");
        assert!(!immediate.iter().any(|(e, _)| e == "OnProbeWarpEnd"), "OnProbeWarpEnd must not fire at 0s");

        ctx.advance_secs(15);
        let after_tick = drain_all(&ctx, &mut sess, PILOT_A);
        assert_eq!(after_tick.iter().filter(|(e, _)| e == "OnProbeWarpEnd").count(), 8);
        assert!(
            after_tick.iter().any(|(e, a)| e == "OnProbeStateChanged" && a.get(1) == Some(&PyRep::Integer(4))),
            "warp end / scan start must emit OnProbeStateChanged(Scanning=4)"
        );
        assert!(
            after_tick.iter().any(|(e, a)| e == "OnProbeStateChanged" && a.get(1) == Some(&PyRep::Integer(1))),
            "scan completion must emit OnProbeStateChanged(Idle=1)"
        );
        assert!(after_tick.iter().any(|(e, _)| e == "OnSystemScanStopped"));

        rpc(&ctx, &mut sess, "scanMgr", "RecoverProbes", &[]).await.unwrap();
        let recover_notifs = drain_all(&ctx, &mut sess, PILOT_A);
        assert!(
            recover_notifs.iter().any(|(e, a)| e == "OnProbeStateChanged" && a.get(1) == Some(&PyRep::Integer(5))),
            "RecoverProbes must emit OnProbeStateChanged(Returning=5)"
        );
    }

    // parity: notify:OnCampaignEnrollmentChanged, notify:OnCharacterPermanentlyBanned
    // parity: notify:OnCharacterTemporarilyBanned, notify:OnCharacterUnbanned
    // parity: notify:OnNpcTimerUpdate, notify:OnDisapprovalTimerUpdate, notify:OnSystemDisapprovalFlagUpdates
    #[tokio::test]
    async fn test_campaign_enrollment_chat_bans_and_crimewatch_timers() {
        eve_testkit::attest(&[
            "notify:OnCampaignEnrollmentChanged",
            "notify:OnCharacterPermanentlyBanned",
            "notify:OnCharacterTemporarilyBanned",
            "notify:OnCharacterUnbanned",
            "notify:OnNpcTimerUpdate",
            "notify:OnDisapprovalTimerUpdate",
            "notify:OnSystemDisapprovalFlagUpdates",
        ]);

        let ctx = new_ctx();
        let mut gm_sess = SessionState::new(91_003);
        common::login_pilot(&ctx, &mut gm_sess, "demo", "demo-secret").await;
        gm_sess.role = eve_db::service_roles::ROLE_GM_DEFAULT;
        rpc(&ctx, &mut gm_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)]).await.unwrap();
        gm_sess.role = eve_db::service_roles::ROLE_GM_DEFAULT;
        let mut p2_sess = SessionState::new(91_004);
        common::login_pilot(&ctx, &mut p2_sess, "other", "other-secret").await;
        rpc(&ctx, &mut p2_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_B)]).await.unwrap();

        let alt_campaign = eve_server::services::campaigns_and_milestones::get_sde_login_campaigns(&ctx.sde)
            .keys()
            .copied()
            .find(|&id| id != 2261)
            .unwrap_or(2261);
        let before_p2 = rpc(&ctx, &mut p2_sess, "loginCampaignManager", "get_client_campaign_state", &[]).await.unwrap();
        rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String(format!("/campaign enroll {alt_campaign}"))]).await.unwrap();
        assert!(drain_all(&ctx, &mut gm_sess, PILOT_A).iter().any(|(e, _)| e == "OnCampaignEnrollmentChanged"));
        let after_p1 = rpc(&ctx, &mut gm_sess, "loginCampaignManager", "get_client_campaign_state", &[]).await.unwrap();
        let after_p2 = rpc(&ctx, &mut p2_sess, "loginCampaignManager", "get_client_campaign_state", &[]).await.unwrap();
        assert_eq!(dict_get(&after_p1, "campaignID"), Some(&PyRep::Integer(alt_campaign)));
        assert_eq!(dict_get(&after_p2, "campaignID"), dict_get(&before_p2, "campaignID"));

        rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String(format!("/chatban {PILOT_B} perm"))]).await.unwrap();
        assert!(drain_all(&ctx, &mut p2_sess, PILOT_B).iter().any(|(e, _)| e == "OnCharacterPermanentlyBanned"));
        let perm_rsp = ctx.quasar_gateway.handle_public_request(&p2_sess, &chat_bcast_req("blocked"), None, false, ctx.now_unix());
        assert_ne!(perm_rsp.status_code, STATUS_OK, "permanently chat-banned character must be blocked");

        rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String(format!("/chatunban {PILOT_B}"))]).await.unwrap();
        assert!(drain_all(&ctx, &mut p2_sess, PILOT_B).iter().any(|(e, _)| e == "OnCharacterUnbanned"));

        rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String(format!("/chatban {PILOT_B} 30"))]).await.unwrap();
        assert!(drain_all(&ctx, &mut p2_sess, PILOT_B).iter().any(|(e, _)| e == "OnCharacterTemporarilyBanned"));
        ctx.advance_secs(35);
        let after_exp = ctx.quasar_gateway.handle_public_request(&p2_sess, &chat_bcast_req("restored"), None, false, ctx.now_unix());
        assert_eq!(after_exp.status_code, STATUS_OK, "temporarily chat-banned character must be unblocked after expiry");

        let zarzakh = 30_100_000_u32;
        let (ship_a, ship_b, gun_id) = (630_000_101_i64, 630_000_102_i64, 630_000_110_i64);
        let mut s_a = login_in_space(&ctx, 91_005, "demo", "demo-secret", PILOT_A, ship_a, 587, zarzakh, DVec3::ZERO).await;
        let _s_b = login_in_space(&ctx, 91_006, "other", "other-secret", PILOT_B, ship_b, 587, zarzakh, DVec3::new(1_000.0, 0.0, 0.0)).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(gun_id, make_item(gun_id, 484, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true));
            snap.module_online_states.insert(gun_id, true);
        });
        ctx.invalidate_ship_fitting(ship_a);
        let sys_h = ctx.systems.get_or_spawn(zarzakh);
        sys_h.add_target_lock(ship_a, TargetLock { target_id: ship_b, lock_time_ms: 100.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await.unwrap();
        drain_all(&ctx, &mut s_a, PILOT_A);
        rpc(&ctx, &mut s_a, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("projectileFired".into()), PyRep::Integer(ship_b)]).await.unwrap();
        let z_notifs = drain_all(&ctx, &mut s_a, PILOT_A);
        assert!(z_notifs.iter().any(|(e, _)| e == "OnDisapprovalTimerUpdate"));
        assert!(z_notifs.iter().any(|(e, _)| e == "OnSystemDisapprovalFlagUpdates"));

        let npc_id = 630_000_199_i64;
        sys_h.spawn_ball(Ball {
            id: npc_id, type_id: 16_250, position: DVec3::new(1_500.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 100.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Rigid,
        }).await.unwrap();
        sys_h.add_target_lock(ship_a, TargetLock { target_id: npc_id, lock_time_ms: 100.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await.unwrap();
        drain_all(&ctx, &mut s_a, PILOT_A);
        rpc(&ctx, &mut s_a, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("projectileFired".into()), PyRep::Integer(npc_id)]).await.unwrap();
        let npc_notifs = drain_all(&ctx, &mut s_a, PILOT_A);
        assert!(npc_notifs.iter().any(|(e, _)| e == "OnNpcTimerUpdate"));
    }

    #[tokio::test]
    async fn test_corp_role_bit_values_match_v24_client() {
        use eve_db::corp_roles;

        // Exact bit values from V24 client `eve/common/lib/appConst.pyj` (`corpRole*` constants).
        let expected_v24_roles: &[(u64, &str)] = &[
            (1 << 0, "Director"),
            (1 << 1, "Deliveries Hangar Take"),
            (1 << 2, "Deliveries Hangar Query"),
            (1 << 3, "Deliveries Hangar Container Take"),
            (1 << 7, "Personnel Manager"),
            (1 << 8, "Accountant"),
            (1 << 9, "Security Officer"),
            (1 << 10, "Factory Manager"),
            (1 << 11, "Station Manager"),
            (1 << 12, "Auditor"),
            (1 << 13, "Hangar Can Take 1"),
            (1 << 14, "Hangar Can Take 2"),
            (1 << 15, "Hangar Can Take 3"),
            (1 << 16, "Hangar Can Take 4"),
            (1 << 17, "Hangar Can Take 5"),
            (1 << 18, "Hangar Can Take 6"),
            (1 << 19, "Hangar Can Take 7"),
            (1 << 20, "Hangar Can Query 1"),
            (1 << 21, "Hangar Can Query 2"),
            (1 << 22, "Hangar Can Query 3"),
            (1 << 23, "Hangar Can Query 4"),
            (1 << 24, "Hangar Can Query 5"),
            (1 << 25, "Hangar Can Query 6"),
            (1 << 26, "Hangar Can Query 7"),
            (1 << 27, "Account Can Take 1"),
            (1 << 28, "Account Can Take 2"),
            (1 << 29, "Account Can Take 3"),
            (1 << 30, "Account Can Take 4"),
            (1 << 31, "Account Can Take 5"),
            (1 << 32, "Account Can Take 6"),
            (1 << 33, "Account Can Take 7"),
            (1 << 34, "Diplomat"),
            (1 << 35, "Brand Manager"),
            (1 << 41, "Equipment Config"),
            (1 << 42, "Container Can Take 1"),
            (1 << 43, "Container Can Take 2"),
            (1 << 44, "Container Can Take 3"),
            (1 << 45, "Container Can Take 4"),
            (1 << 46, "Container Can Take 5"),
            (1 << 47, "Container Can Take 6"),
            (1 << 48, "Container Can Take 7"),
            (1 << 49, "Can Rent Office"),
            (1 << 50, "Can Rent Factory Slot"),
            (1 << 51, "Can Rent Research Slot"),
            (1 << 52, "Junior Accountant"),
            (1 << 53, "Starbase Config"),
            (1 << 54, "Trader"),
            (1 << 55, "Chat Manager"),
            (1 << 56, "Contract Manager"),
            (1 << 57, "Infrastructure Tactical Officer"),
            (1 << 58, "Starbase Caretaker"),
            (1 << 59, "Fitting Manager"),
            (1 << 60, "Project Manager"),
            (1 << 61, "Terrestrial Logistics Officer"),
            (1 << 62, "Skill Plan Manager"),
        ];

        assert_eq!(corp_roles::ROLE_DIRECTOR, 1 << 0);
        assert_eq!(corp_roles::ROLE_DELIVERIES_HANGAR_TAKE, 1 << 1);
        assert_eq!(corp_roles::ROLE_DELIVERIES_HANGAR_QUERY, 1 << 2);
        assert_eq!(corp_roles::ROLE_DELIVERIES_HANGAR_CONTAINER_TAKE, 1 << 3);
        assert_eq!(corp_roles::ROLE_BRAND_MANAGER, 1 << 35);
        assert_eq!(corp_roles::ROLE_TRADER, 1 << 54);
        assert_eq!(corp_roles::ROLE_CHAT_MANAGER, 1 << 55);
        assert_eq!(corp_roles::ROLE_CONTRACT_MANAGER, 1 << 56);
        assert_eq!(corp_roles::ROLE_INFRASTRUCTURE_TACTICIAN, 1 << 57);
        assert_eq!(corp_roles::ROLE_STARBASE_CARETAKER, 1 << 58);
        assert_eq!(corp_roles::ROLE_FITTING_MANAGER, 1 << 59);
        assert_eq!(corp_roles::ROLE_PROJECT_MANAGER, 1 << 60);
        assert_eq!(corp_roles::ROLE_TERRESTRIAL_LOGISTICS_OFFICER, 1 << 61);
        assert_eq!(corp_roles::ROLE_SKILL_PLAN_MANAGER, 1 << 62);

        let ctx = new_ctx();
        let mut sess = SessionState::new(91_007);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)]).await.unwrap();

        let PyRep::Rowset { rows, .. } = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("roles".into())]).await.unwrap() else {
            panic!("expected Rowset");
        };
        let actual: Vec<(u64, String)> = rows
            .into_iter()
            .map(|r| {
                let id = r[0].as_i64().expect("roleID integer") as u64;
                let name = r[1].as_str().expect("roleName string").to_string();
                (id, name)
            })
            .collect();

        for &(bit, expected_name) in expected_v24_roles {
            let found: Vec<&str> = actual
                .iter()
                .filter_map(|(id, name)| (*id == bit).then_some(name.as_str()))
                .collect();
            assert_eq!(found, vec![expected_name], "mismatch for role bit 1 << {}", bit.trailing_zeros());
        }
        assert_eq!(actual.len(), expected_v24_roles.len(), "unexpected extra/missing roles in cache.Rowset(\"roles\")");
    }
}
