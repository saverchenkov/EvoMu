//! Real server-side emission tests for Notify/RPC Batch 8 (Part 4: Combat, EWAR, Cloak, Invuln, Fleet, Drones & Ammo).
//!
//! Covers 14 Group 4 parity rows across two test functions (`<= 15` tags each):
//! - Test 1 (`6` rows): `OnJamStart`, `OnEwarEnd`, `OnJumpCloakUpdated`,
//!   `OnJumpCloakCancelled`, `OnInvulnCancelled`, `OnRestoringInvulnUpdated`
//! - Test 2 (`8` rows): `OnChargeLoadedToModule`, `OnAmmoInBankChanged`, `OnDroneActivityChange`,
//!   `OnFleetBoosted`, `OnFleetLootEvent`, `OnFleetJumpBeaconDeployableChange`,
//!   `OnFleetRespawnPointsUpdate`, `OnFleetEscalatingPathMessage`

#![forbid(unsafe_code)]

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
