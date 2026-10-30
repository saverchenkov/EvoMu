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

pub(crate) async fn lock_target(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    ship_id: i64,
    target_id: i64,
) {
    rpc(ctx, sess, "dogmaIM", "AddTarget", &[PyRep::Integer(target_id)])
        .await
        .expect("AddTarget");
    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;
}

pub(crate) fn make_item(
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

pub(crate) async fn login_in_space(
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
    if char_id == PILOT_C {
        ctx.db.with_mut_snapshot(|snap| {
            snap.accounts.insert(
                "third".to_string(),
                eve_db::AccountRecord {
                    user_id: 3,
                    username: "third".to_string(),
                    password_token: eve_db::WorldRepository::hash_password_token("third", "third-secret"),
                    role: eve_machonet::service_roles::ROLE_PLAYER_DEFAULT,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            let mut template = snap.characters[&PILOT_A].clone();
            template.character_id = PILOT_C;
            template.user_id = 3;
            template.name = "Third Capsuleer".to_string();
            snap.characters.insert(PILOT_C, template);
        });
    }
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    rpc(ctx, &mut s, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    ctx.db.with_mut_snapshot(|snap| {
        let template = snap.characters[&PILOT_A].clone();
        let ch = snap.characters.entry(char_id).or_insert_with(|| template);
        ch.character_id = char_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.items.insert(
            ship_id,
            make_item(ship_id, ship_type, char_id, sys_id as i64, InventoryFlag::None, 1, true),
        );
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
    let _ = handle
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: ship_type,
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
            slim,
        )
        .await;
    ctx.advance_secs(12);
    let _ = drain_all(ctx, &mut s, char_id);
    s
}

pub(crate) fn drain_all(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
) -> Vec<(String, Vec<PyRep>)> {
    let mut out: Vec<(String, Vec<PyRep>)> = ctx
        .drain_notifications_for_char(char_id)
        .into_iter()
        .map(|n| (n.event, n.args))
        .collect();
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
                id: ship_a,
                type_id: 32880,
                position: ship_near_poch_moon,
                velocity: (moon_poch_pos - ship_near_poch_moon).normalize() * 100.0,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_200_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
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
                id: ship_a,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
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
    let dmg_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    let dmg_args = find_notif(&dmg_notifs_b, "OnDamageStateChange");
    assert_eq!(dmg_args.len(), 2);
    assert_eq!(dmg_args[0], PyRep::Integer(ship_b));
    let PyRep::Tuple(ds) = &dmg_args[1] else { panic!("expected damageState tuple") };
    assert_eq!(ds.len(), 3);
    let PyRep::Tuple(sh_tup) = &ds[0] else { panic!("expected shield tuple") };
    assert_eq!(sh_tup.len(), 3);
    let (sh_r, ar_r, hu_r) = (sh_tup[0].as_f64().unwrap(), ds[1].as_f64().unwrap(), ds[2].as_f64().unwrap());
    assert!((0.0..1.0).contains(&sh_r) && ar_r == 1.0 && hu_r == 1.0);

    // Pod Pilot C via weapon fire with bound beyonce object -> 3-arg OnMachoObjectDisconnect
    let bind_beyonce = ctx
        .dispatch_rpc(
            &mut sess_c,
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Tuple(vec![PyRep::Integer(SYS_JITA as i64), PyRep::Integer(6)]), PyRep::None],
            &HashMap::new(),
            Some("machoNet:26000"),
        )
        .await
        .expect("MachoBindObject beyonce");
    let PyRep::Tuple(bind_parts) = bind_beyonce else { panic!("MachoBindObject tuple") };
    let PyRep::SubStruct(sub_s) = &bind_parts[0] else { panic!("SubStruct OID") };
    let PyRep::SubStream(sub_t) = sub_s.as_ref() else { panic!("SubStream OID") };
    let PyRep::Tuple(oid_pair) = sub_t.as_ref() else { panic!("(oid, refID) tuple") };
    assert!(oid_pair[0].as_str().unwrap_or("").starts_with("N=888444:"));
    assert!(oid_pair[1].as_i64().unwrap_or(0) > 116_444_736_000_000_000);
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
                id: pod_c,
                type_id: 670,
                position: DVec3::new(2_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 50_000.0,
                radius: 25.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            pod_cs,
            ctx.build_ship_slim_meta(PILOT_C, 670),
        )
        .await;
    lock_target(&ctx, &mut sess_a, SYS_JITA, ship_a, pod_c).await;
    let _ = drain_all(&ctx, &mut sess_c, PILOT_C);
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(a_gun_id), PyRep::String("projectileFired".into()), PyRep::Integer(pod_c)])
        .await
        .expect("fire at pod_c");
    let pod_notifs = drain_all(&ctx, &mut sess_c, PILOT_C);
    let disc_args = find_notif(&pod_notifs, "OnMachoObjectDisconnect");
    let macho_client_c = (1_000_000_000_000_u64 + sess_c.session_id) as i64;
    assert_eq!(disc_args, &[oid_pair[0].clone(), PyRep::Integer(macho_client_c), oid_pair[1].clone()]);

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
                is_cloaked: false,
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
