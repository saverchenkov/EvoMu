//! Real production server-to-client `notify:On*` emission integration tests (Batch 10, Part 4).
//!
//! Covers ESS Reserve Bank inventory full (`OnEssReserveBankInventoryFull`), FW complex capture
//! ticks (`OnCaptureChanged`), Hacking tile selection (`OnLocalHackProgressUpdated`), Pirate
//! Insurgency Stage-5 security drop (`OnSecurityModified`), Null-Sec Dynamic Bounty multiplier
//! updates (`OnSystemDynamicBountyStateChanged`), and multi-pilot asteroid mining
//! (`OnOreMinedByOther`) via live RPCs and `tick_background_cluster`.

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, MovementMode};
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
        .spawn_ball_with_combat(make_ball(ship_id, ship_type_id, DVec3::new(1000.0, 0.0, 0.0)), Default::default())
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
    let pirate_npc_tid = *ctx
        .sde
        .types
        .iter()
        .find(|(_, t)| t.category_id == 11 && t.default_attributes.get(&481).copied().unwrap_or(0.0) >= 100_000.0)
        .map(|(id, _)| id)
        .expect("pirate NPC with bounty");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            700_530,
            make_item(700_530, 487, PILOT_A, 700_501, InventoryFlag::HiSlot0, 1, true),
        );
        snap.items.insert(
            700_531,
            make_item(700_531, 178, PILOT_A, 700_501, InventoryFlag::HiSlot0, 100, false),
        );
        for &sk in &[3300_u32, 3302] {
            snap.skills.entry(PILOT_A).or_default().insert(
                sk,
                eve_db::TrainedSkillRecord { type_id: sk, level: 5, skill_points: 256_000.0 },
            );
        }
    });
    ctx.invalidate_ship_fitting(700_501);
    let _ = rpc(&ctx, &mut sess_a, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(700_501), PyRep::Integer(700_530)]).await;
    let _ = null_handle
        .spawn_ball_with_combat(
            make_ball(900_550, pirate_npc_tid, DVec3::new(1_200.0, 0.0, 0.0)),
            CombatState {
                shield_hp: 500_000.0,
                max_shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                max_armor_hp: 500_000.0,
                structure_hp: 500_000.0,
                max_structure_hp: 500_000.0,
                ..Default::default()
            },
        )
        .await;
    let _ = null_handle
        .spawn_ball_with_combat(
            make_ball(900_551, pirate_npc_tid, DVec3::new(1_250.0, 0.0, 0.0)),
            CombatState {
                shield_hp: 0.0,
                max_shield_hp: 100.0,
                armor_hp: 0.0,
                max_armor_hp: 100.0,
                structure_hp: 1.0,
                max_structure_hp: 100.0,
                ..Default::default()
            },
        )
        .await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    // Negative: shooting 500k-HP NPC does NOT destroy it -> zero OnSystemDynamicBountyStateChanged
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(900_550)])
        .await
        .unwrap();
    assert!(!has_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnSystemDynamicBountyStateChanged"));

    for _ in 0..10 {
        null_handle.step_manual(1.0).await.unwrap();
    }
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    // Positive: destroying 1-HP pirate NPC in Null-Sec adjusts DBS rate and emits OnSystemDynamicBountyStateChanged
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(900_551)])
        .await
        .unwrap();
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
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!has_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnCaptureChanged"));

    // Positive: move Pilot A into capture pocket -> tick_background_cluster emits OnCaptureChanged
    let pocket = eve_server::services::fw_gameplay::pocket_pos_for_complex(fw_gate_id);
    let _ = low_handle.set_ball_position(700_501, pocket).await;
    ctx.advance_secs(5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
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
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!has_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnSecurityModified"));

    // Positive: move Pilot A to [500, 0, 0] -> tick advances corruption 799 -> >=800 (Stage 5) -> OnSecurityModified
    let _ = low_handle.set_ball_position(700_501, DVec3::new(500.0, 0.0, 0.0)).await;
    ctx.advance_secs(5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
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
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!has_notif(&drain_all(&ctx, &mut sess_obs, PILOT_B), "OnOreMinedByOther"));

    // Positive: after cycle completes, observer PILOT_B gets OnOreMinedByOther(rock_id, qty), while miner PILOT_A does NOT
    ctx.advance_secs(65);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let miner_notifs = drain_all(&ctx, &mut sess_jita, PILOT_A);
    assert!(!has_notif(&miner_notifs, "OnOreMinedByOther"));
    let obs_notifs = drain_all(&ctx, &mut sess_obs, PILOT_B);
    let ore_args = find_notif(&obs_notifs, "OnOreMinedByOther");
    assert_eq!(ore_args.len(), 2);
    assert_eq!(ore_args[0], PyRep::Integer(rock_id));
    assert!(ore_args[1].as_i64().unwrap_or(0) > 0);
}
