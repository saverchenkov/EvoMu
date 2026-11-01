//! Live integration tests for Batch 23 `notify:` closure (12 notifications).

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::notify_emitters_b23::{
    JUMP_QUEUE_LIMITS_BLOB_KEY, JUMP_QUEUE_STATE_BLOB_KEY,
};
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
async fn b23_fleet_escalation_decloak_and_jump_queue_notifications() {
    let mut ctx = ServerContext::new();
    std::sync::Arc::make_mut(&mut ctx.sde).jumps.insert(
        50_001_248,
        eve_sde::StargateJump {
            stargate_id: 50_001_248,
            from_solar_system_id: SYS_JITA,
            to_solar_system_id: SYS_PERIMETER,
            destination_stargate_id: 50_001_249,
            position: DVec3::ZERO,
        },
    );
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
    assert_eq!(fleet_esc[1].as_i64(), Some(2002));

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
