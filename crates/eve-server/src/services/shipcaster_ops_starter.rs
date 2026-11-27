use eve_db::lock_rank::{AnyRwLock, RankedMutex, RANK_LEAF};
use eve_machonet::SessionState;
use eve_proto::BridgeRpcError;

use super::{
    is_capital_or_freighter_group, MAX_LINKED_LANDING_PADS_PER_FACTION,
    STARTER_SHIPCASTER_MAX_ROOKIE_AGE_SECS,
};
use crate::router::cache_notifications::PendingMachoNotification;
use crate::router::ServerContext;
use crate::services::cluster_macho_misc_services::with_misc_state;

fn rpc_err(code: &str, msg: &str) -> BridgeRpcError {
    BridgeRpcError {
        code: code.to_string(),
        message: msg.to_string(),
        client_error: None,
    }
}

/// Validates all Rookie Starter Shipcaster eligibility rules:
/// 1. One-time-use per character (`used_starter_shipcaster_chars`).
/// 2. Rookie character age `<= 30` days (`create_date_unix`).
/// 3. No active Crimewatch weapons, PvP combat, or criminal timer.
/// 4. Subcapital, non-capsule active ship.
pub fn check_starter_shipcaster_eligibility(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
) -> Result<(), BridgeRpcError> {
    let already_used = with_misc_state(ctx, |st| st.used_starter_shipcaster_chars.contains(&char_id));
    if already_used {
        return Err(rpc_err(
            "StarterShipcasterAlreadyUsed",
            "Rookie starter shipcasters may only be used once per character",
        ));
    }

    let now = ctx.now_unix();
    let (create_date_unix, active_ship_id) = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .map(|ch| (ch.create_date_unix, session.ship_id.unwrap_or(ch.active_ship_id)))
            .unwrap_or((0, 0))
    });
    if create_date_unix == 0
        || now.saturating_sub(create_date_unix) > STARTER_SHIPCASTER_MAX_ROOKIE_AGE_SECS
    {
        return Err(rpc_err(
            "CharacterTooOldForStarterShipcaster",
            "Only rookie characters 30 days old or younger may use a Starter Shipcaster",
        ));
    }

    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st_cw = cw.get_or_create(char_id);
        let timer_kind = if st_cw.is_weapon_timer_active(now) {
            Some("weapons")
        } else if st_cw.is_pvp_active(now) {
            Some("pvp")
        } else if st_cw.criminal_flag_until_unix > now {
            Some("criminal")
        } else {
            None
        };
        if let Some(timer_kind) = timer_kind {
            return Err(BridgeRpcError::weapons_timer_active(
                char_id,
                "starter_shipcaster_jump",
                timer_kind,
                true,
                "Cannot jump through starter shipcaster while a combat or weapons timer is active",
            ));
        }
    }

    if active_ship_id <= 0 {
        return Err(rpc_err("CustomError", "Starter shipcaster requires an active ship"));
    }
    let ship_type_id = ctx
        .db
        .with_snapshot(|s| s.items.get(&active_ship_id).map(|i| i.type_id))
        .unwrap_or(0);
    if matches!(ship_type_id, 670 | 33328) {
        return Err(rpc_err(
            "CapsuleCannotJumpShipcaster",
            "Capsules cannot jump through a starter shipcaster",
        ));
    }
    let ship_group_id = ctx.sde.types.get(&ship_type_id).map(|t| t.group_id).unwrap_or(0);
    if is_capital_or_freighter_group(ship_group_id) {
        return Err(rpc_err(
            "CapitalShipCannotJumpShipcaster",
            "Capital ships and freighters cannot jump through a starter shipcaster",
        ));
    }
    Ok(())
}

pub(crate) fn emit_shipcaster_target_state_changed_core(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<
        &dyn AnyRwLock<std::collections::HashMap<i64, Vec<PendingMachoNotification>>>,
    >,
    notify_tx: &tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>,
    solar_system_id: u32,
    item_id: i64,
) {
    let Some(pws) = pending_notifications else {
        return;
    };
    let recipients: Vec<(i64, Option<u64>)> = db.with_snapshot(|snap| {
        snap.characters
            .values()
            .filter(|c| {
                c.solar_system_id == solar_system_id
                    && db
                        .get_online_session(c.user_id)
                        .is_some_and(|os| os.character_id == Some(c.character_id))
            })
            .map(|c| (c.character_id, Some(c.user_id)))
            .collect()
    });
    if recipients.is_empty() {
        return;
    }
    let now_ft = db.clock().now_blueos_filetime_ticks();
    for (cid, uid) in recipients {
        crate::router::notify_registry::emit_typed_target_to_char_with_id_type(
            Some(pws),
            Some(notify_tx),
            cid,
            uid,
            <crate::router::notify_registry::OnShipcasterTargetStateChanged as crate::router::notify_registry::TypedNotification>::DEFAULT_ID_TYPE,
            now_ft,
            crate::router::notify_registry::OnShipcasterTargetStateChanged { item_id },
        );
    }
}

/// Live combat ball-destruction hook (`router/context.rs`): disrupts/destroys a destroyed FW
/// landing pad and promotes the next queued built pad for that faction if available.
pub async fn on_landing_pad_ball_destroyed(ctx: &ServerContext, ball_id: i64) {
    on_landing_pad_ball_destroyed_core(
        &ctx.cluster_misc_state,
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        Some(&ctx.pending_notifications),
        &ctx.notify_tx,
        ball_id,
    )
    .await;
}

pub async fn on_landing_pad_ball_destroyed_core(
    misc: &std::sync::Arc<
        RankedMutex<
            crate::services::cluster_macho_misc_services::ClusterMiscRuntimeState,
            RANK_LEAF,
        >,
    >,
    systems: &std::sync::Arc<crate::system_actor::SystemRegistry>,
    db: &std::sync::Arc<eve_db::WorldRepository>,
    sde: &eve_sde::SdeRegistry,
    pending_notifications: Option<
        &dyn AnyRwLock<std::collections::HashMap<i64, Vec<PendingMachoNotification>>>,
    >,
    notify_tx: &tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>,
    ball_id: i64,
) {
    let now_unix = db.clock().now_unix_secs();
    let destroyed_info = crate::services::cluster_macho_misc_services::with_misc_state_arc(misc, sde, |st| {
        let pad_idx = st.landing_pads.iter().position(|p| p.item_id == ball_id && p.faction_id > 0)?;
        let pad = st.landing_pads.remove(pad_idx);
        let mut launcher_info = None;
        if let Some(launcher) = st.launchers.iter_mut().find(|l| l.faction_id == pad.faction_id) {
            launcher.linked_pad_ids.retain(|&id| id != ball_id);
            if launcher.linked_pad_ids.len() < MAX_LINKED_LANDING_PADS_PER_FACTION {
                if let Some(next_pad) = st
                    .landing_pads
                    .iter_mut()
                    .find(|p| p.faction_id == pad.faction_id && p.is_built && !p.is_disrupted && p.link_timestamp == 0)
                {
                    next_pad.link_timestamp = now_unix.max(1) as i64;
                    launcher.linked_pad_ids.push(next_pad.item_id);
                }
            }
            launcher_info = Some((launcher.solar_system_id, launcher.item_id));
        }
        Some((pad.solar_system_id, launcher_info))
    });
    let Some((pad_sys, launcher_opt)) = destroyed_info else {
        return;
    };
    crate::services::cluster_macho_misc_services::persist_misc_state_arc(misc, sde, db);
    let _ = systems.get_or_spawn(pad_sys).remove_ball(ball_id).await;
    super::emit_shipcaster_target_state_changed_core(db, pending_notifications, notify_tx, pad_sys, ball_id);
    if let Some((war_hq_sys, launcher_id)) = launcher_opt {
        super::emit_shipcaster_target_state_changed_core(db, pending_notifications, notify_tx, war_hq_sys, launcher_id);
    }
}

use glam::DVec3;
use crate::system_actor::SystemHandle;
use eve_destiny::MovementMode;

/// Computes the 3D launch trajectory vector between origin launcher and destination landing pad / beacon.
/// If across solar systems, computes celestial unit direction between system coordinates in the galaxy (`feat:navigation.shipcaster`).
pub fn calculate_shipcaster_launch_vector(
    sde: &eve_sde::SdeRegistry,
    origin_sys: u32,
    launcher_pos: DVec3,
    dest_sys: u32,
    pad_pos: DVec3,
) -> DVec3 {
    if origin_sys == dest_sys {
        let diff = pad_pos - launcher_pos;
        if diff.length_squared() > 1e-6 {
            return diff.normalize();
        }
    } else if let (Some(orig_sys_rec), Some(dest_sys_rec)) = (
        sde.solar_systems.get(&origin_sys),
        sde.solar_systems.get(&dest_sys),
    ) {
        let gal_dir = DVec3::new(
            dest_sys_rec.galactic_pos_ly[0] - orig_sys_rec.galactic_pos_ly[0],
            dest_sys_rec.galactic_pos_ly[1] - orig_sys_rec.galactic_pos_ly[1],
            dest_sys_rec.galactic_pos_ly[2] - orig_sys_rec.galactic_pos_ly[2],
        );
        if gal_dir.length_squared() > 1e-6 {
            return gal_dir.normalize();
        }
    }
    DVec3::X
}

/// Computes the exact beacon arrival position and landing offset along the launch trajectory (`feat:navigation.shipcaster`).
pub fn calculate_shipcaster_arrival_position(
    pad_pos: DVec3,
    launch_vector: DVec3,
    standoff_m: f64,
) -> DVec3 {
    pad_pos + launch_vector * standoff_m
}

/// Executes beacon alignment and spool-up special FX on the ship in the origin ballpark (`feat:navigation.shipcaster`, `feat:navigation.starter-shipcaster`).
pub async fn apply_shipcaster_beacon_alignment_and_spool(
    sys: &SystemHandle,
    ship_id: i64,
    launcher_id: i64,
    launch_vector: DVec3,
) -> Result<(), BridgeRpcError> {
    let _ = sys
        .set_movement_mode(
            ship_id,
            MovementMode::GotoDirection {
                direction: launch_vector,
            },
        )
        .await;
    let _ = sys
        .emit_destiny_event(
            ship_id,
            eve_destiny::DestinyEvent::OnSpecialFX {
                ball_id: ship_id,
                module_id: None,
                guid: "effects.ShipcasterSpool".into(),
                is_active: true,
            },
        )
        .await;
    let _ = sys
        .emit_destiny_event(
            launcher_id,
            eve_destiny::DestinyEvent::OnSpecialFX {
                ball_id: launcher_id,
                module_id: None,
                guid: "effects.ShipcasterLaunch".into(),
                is_active: true,
            },
        )
        .await;
    Ok(())
}

/// Emits arrival landing FX at the destination landing pad / beacon (`feat:navigation.shipcaster`).
pub async fn apply_shipcaster_arrival_fx(sys: &SystemHandle, ship_id: i64) {
    let _ = sys
        .emit_destiny_event(
            ship_id,
            eve_destiny::DestinyEvent::OnSpecialFX {
                ball_id: ship_id,
                module_id: None,
                guid: "effects.ShipcasterLand".into(),
                is_active: true,
            },
        )
        .await;
}

/// Enforces fail-closed operational range and ball presence in the active solar system actor.
pub async fn check_proximity(
    sys: &SystemHandle,
    ship_id: i64,
    target_id: i64,
    max_dist: f64,
    label: &str,
) -> Result<(), BridgeRpcError> {
    if target_id <= 0 {
        return Err(rpc_err(
            "TargetNotFound",
            &format!("Invalid target ID {target_id} for {label}"),
        ));
    }
    let ship_ball = sys
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| rpc_err("DestinyQueryFailed", &e))?
        .map(|(_, b)| b)
        .ok_or_else(|| {
            BridgeRpcError::ship_not_in_space(
                ship_id,
                None,
                Some(sys.solar_system_id),
                "shipcaster_jump",
                "Active ship ball is not in space in this solar system",
            )
        })?;

    let target_ball = sys
        .get_ball_snapshot(target_id)
        .await
        .map_err(|e| rpc_err("DestinyQueryFailed", &e))?
        .map(|(_, b)| b)
        .ok_or_else(|| {
            rpc_err(
                "TargetNotInSpace",
                &format!("{label} entity {target_id} is not present in space in this solar system"),
            )
        })?;

    let dist = ship_ball.position.distance(target_ball.position);
    if dist > max_dist {
        return Err(rpc_err(
            "OutsideOperationalRange",
            &format!("Ship is {dist:.1}m from {label} (must be <= {max_dist:.0}m)"),
        ));
    }
    Ok(())
}

pub async fn prevalidate_shipcaster_jump(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    origin_sys: u32,
) -> Result<(u64, eve_destiny::Ball, Option<eve_destiny::CombatState>), BridgeRpcError> {
    let now_unix = ctx.now_unix();
    let old_handle = ctx.systems.get_or_spawn(origin_sys);
    let Some((cur_tick, ship_ball)) = old_handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| rpc_err("DestinyQueryFailed", &e))?
    else {
        return Err(BridgeRpcError::ship_not_in_space(
            ship_id,
            Some(char_id),
            Some(origin_sys),
            "shipcaster_jump",
            "Active ship ball not in space in origin solar system",
        ));
    };

    if matches!(ship_ball.type_id, 670 | 33328) {
        return Err(rpc_err(
            "CapsuleCannotJumpShipcaster",
            "Capsules cannot jump through a shipcaster",
        ));
    }

    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        if cw.get_or_create(char_id).is_weapon_timer_active(now_unix) {
            return Err(BridgeRpcError::weapons_timer_active(
                char_id,
                "shipcaster_jump",
                "weapons",
                true,
                "Cannot jump through shipcaster while weapons timer is active",
            ));
        }
    }

    if let Some(active_timer) = crate::services::space_destiny::check_session_change_timer_active(
        ctx,
        char_id,
        now_unix,
        Some((origin_sys, cur_tick)),
    ) {
        return Err(rpc_err(
            "SessionChangeInProgress",
            &format!(
                "Cannot jump shipcaster while 10s session-change timer ({}) is active",
                active_timer.reason
            ),
        ));
    }

    let validated = ctx
        .validate_ship_movement(
            char_id,
            ship_id,
            crate::services::space_nav_movement_guard::MovementIntent::ShipcasterJump {
                from_sys: origin_sys,
                to_sys: origin_sys,
            },
        )
        .await?;

    Ok((cur_tick, ship_ball, validated.combat_state))
}

/// Executes jump handoff across system actors, moving the ball and emitting complete session changes.
#[allow(clippy::too_many_arguments)]
pub async fn execute_shipcaster_jump(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    origin_sys: u32,
    dest_sys: u32,
    arrival_pos: DVec3,
    launch_vector: DVec3,
) -> Result<(), BridgeRpcError> {
    let now_unix = ctx.now_unix();
    let (_, _, pre_cs) =
        prevalidate_shipcaster_jump(ctx, char_id, ship_id, origin_sys).await?;
    let saved_heat = ctx.snapshot_rack_heat_and_damage(ship_id);
    let has_damage = saved_heat
        .as_ref()
        .is_some_and(|(_, dmg)| !dmg.is_empty());

    let (outcome, ()) =
        crate::domain::transitions::relocate_character_in_space_with_pre_spawn(
            ctx,
            Some(session),
            crate::domain::transitions::RelocateInSpaceRequest {
                char_id,
                ship_id,
                old_sys_id: origin_sys,
                old_dock: None,
                dest_sys_id: dest_sys,
                was_in_space: true,
                arrival_position: Some(arrival_pos),
                fallback_arrival_position: arrival_pos,
                arrival_velocity: launch_vector * 100.0,
                require_take_ball: true,
                take_ball_missing_code: "SystemTransitionFailed",
                db_transition_failed_code: "SystemTransitionFailed",
                destination_wake:
                    crate::domain::transitions::DestinationWakePolicy::SessionArrival(
                        crate::system_actor::SessionArrivalWakeReason::Jump,
                    ),
                transition_kind: crate::domain::transitions::SpaceTransitionKind::JumpSystem,
                transition_reason: "shipcaster",
                combat_state_preservation:
                    crate::domain::transitions::CombatStatePreservation::PreserveClamped {
                        fallback: pre_cs,
                    },
                gate_cloak_invuln:
                    crate::domain::transitions::GateCloakInvulnPolicy::standard_jump_cloak(),
                cleanup: crate::domain::transitions::LeaveSpaceCleanupPolicy::gameplay_jump(),
                sync_local_chat: true,
                persist_item_position: false,
                record_session_change_timer: true,
                record_solar_system_jump: true,
                bind_beyonce: true,
                push_set_state: false,
            },
            || {
                if has_damage {
                    ctx.db.with_mut_snapshot(|snap| {
                        if let Some((_, ref dmg_list)) = saved_heat {
                            for &(mid, dhp) in dmg_list {
                                if let Some(it) = snap.items.get_mut(&mid) {
                                    it.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, dhp);
                                }
                            }
                        }
                    });
                    ctx.invalidate_ship_fitting_cache_only(ship_id);
                }
                if let Some((rack_heat, _)) = saved_heat {
                    if rack_heat.iter().any(|&h| h.is_finite() && h > 0.0) {
                        ctx.update_ship_rack_heat(char_id, ship_id, rack_heat, false);
                    }
                }
            },
        )
        .await
        .map_err(crate::services::domain_err_to_rpc)?;

    let cleared_zarzakh_gate = if origin_sys == 30_100_000 {
        ctx.db
            .with_mut_snapshot(|snap| snap.zarzakh_gate_locks.remove(&char_id).map(|(gid, _)| gid))
    } else {
        None
    };

    if let Some(locked_gate_id) = cleared_zarzakh_gate {
        crate::services::quasar_sov_services::publish_stargate_lock_deactivated_notice(
            &ctx.quasar_gateway,
            char_id,
            30_100_000,
            locked_gate_id,
            now_unix,
        );
    }

    apply_shipcaster_arrival_fx(&outcome.dest_handle, ship_id).await;
    let _ = outcome.dest_handle.push_set_state(ship_id).await;

    Ok(())
}

#[cfg(test)] #[path = "shipcaster_ops_starter_tests.rs"] mod shipcaster_ops_starter_tests;
#[cfg(test)] #[path = "shipcaster_ops_starter_edges_tests.rs"] mod shipcaster_ops_starter_edges_tests;

#[cfg(test)]
mod transition_regression_tests {
    use super::*;
    use eve_db::{CharacterRecord, InventoryFlag, InventoryItemRecord, TxFaultPlan, WorldRepository};
    use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
    use glam::DVec3;
    use std::sync::Arc;

    struct TempDbDir(std::path::PathBuf);
    impl Drop for TempDbDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn execute_shipcaster_jump_preserves_rollback_on_db_failure_gate_cloak_combat_and_session() {
        let base = ServerContext::new();
        let tmp_dir = TempDbDir(std::env::temp_dir().join(format!(
            "evomu_shipcaster_rollback_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        )));
        let db_path = tmp_dir.0.join("world.sqlite");
        let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite"));
        let ctx = ServerContext::with_sde_and_repository(Arc::clone(&base.sde), db);

        let origin_sys = 30_000_142_u32;
        let dest_sys = 30_000_144_u32;
        let cid = 90_881_001_i64;
        let ship_id = 91_881_001_i64;
        let corp_id = 98_000_001_i64;
        let now_u = ctx.now_unix();

        ctx.db.with_mut_snapshot(|snap| {
            let mut ch = CharacterRecord::default();
            ch.character_id = cid;
            ch.name = "RookieCaster".into();
            ch.corporation_id = corp_id;
            ch.solar_system_id = origin_sys;
            ch.active_ship_id = ship_id;
            ch.create_date_unix = now_u.saturating_sub(3_600);
            ch.station_id = None;
            ch.structure_id = None;
            snap.characters.insert(cid, ch);

            let mut ship = InventoryItemRecord::default();
            ship.item_id = ship_id;
            ship.type_id = 587;
            ship.owner_id = cid;
            ship.location_id = i64::from(origin_sys);
            ship.flag = InventoryFlag::None;
            ship.quantity = 1;
            ship.singleton = true;
            snap.items.insert(ship_id, ship);
        });

        let origin_handle = ctx.systems.get_or_spawn(origin_sys);
        let cs = CombatState {
            shield_hp: 150.0,
            max_shield_hp: 400.0,
            armor_hp: 120.0,
            max_armor_hp: 300.0,
            structure_hp: 90.0,
            max_structure_hp: 300.0,
            capacitor: 80.0,
            max_capacitor: 250.0,
            is_cloaked: false,
            ..Default::default()
        };
        let meta = SlimItemMeta {
            owner_id: cid,
            char_id: Some(cid),
            corp_id,
            alliance_id: -1,
            group_id: 25,
            category_id: 6,
            ..Default::default()
        };
        origin_handle
            .spawn_ball_with_meta(
                Ball {
                    id: ship_id,
                    type_id: 587,
                    position: DVec3::new(1_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 0.5,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
                meta,
            )
            .await
            .expect("spawn origin ship");

        let mut sess = SessionState::new(7701);
        sess.user_id = Some(7701);
        sess.character_id = Some(cid);
        sess.corporation_id = Some(corp_id);
        sess.solar_system_id = Some(origin_sys);
        sess.ship_id = Some(ship_id);

        // Warm crimewatch snapshot state via prevalidation before arming FailAtCommit so the
        // injected DB commit fault lands inside `set_character_solar_system`.
        let _ = prevalidate_shipcaster_jump(&ctx, cid, ship_id, origin_sys)
            .await
            .expect("initial shipcaster prevalidation succeeds");

        // 1. SQLite commit fault must fail cleanly, restore the taken ball in origin, and leave dest cold.
        ctx.db.set_fault_plan_for_test(TxFaultPlan::FailAtCommit);
        let rollback_err = execute_shipcaster_jump(
            &ctx,
            &mut sess,
            cid,
            ship_id,
            origin_sys,
            dest_sys,
            DVec3::new(5_000.0, 0.0, 0.0),
            DVec3::X,
        )
        .await
        .expect_err("DB commit fault must fail shipcaster jump");
        assert_eq!(rollback_err.code, "SystemTransitionFailed");
        assert_eq!(sess.solar_system_id, Some(origin_sys));
        assert!(
            ctx.systems.get_handle(dest_sys).is_none(),
            "destination system must stay cold on DB rollback"
        );
        let (_, restored_ball) = origin_handle
            .get_ball_snapshot(ship_id)
            .await
            .expect("origin query")
            .expect("ship ball must be restored to origin system on DB rollback");
        assert!((restored_ball.position - DVec3::new(1_000.0, 0.0, 0.0)).length() < 1e-6);

        // 2. Clear fault -> shipcaster jump succeeds with preserved vitals, gate cloak/invuln, and session transition.
        ctx.db.set_fault_plan_for_test(TxFaultPlan::None);
        execute_shipcaster_jump(
            &ctx,
            &mut sess,
            cid,
            ship_id,
            origin_sys,
            dest_sys,
            DVec3::new(5_000.0, 0.0, 0.0),
            DVec3::X,
        )
        .await
        .expect("shipcaster jump succeeds once fault cleared");

        assert_eq!(sess.solar_system_id, Some(dest_sys));
        assert_eq!(sess.station_id, None);
        assert!(
            origin_handle.get_ball_snapshot(ship_id).await.unwrap().is_none(),
            "origin ball must be removed after successful jump"
        );
        let dest_handle = ctx
            .systems
            .get_handle(dest_sys)
            .expect("destination system woken");
        let (dest_tick, arrived_ball) = dest_handle
            .get_ball_snapshot(ship_id)
            .await
            .unwrap()
            .expect("arrived ball in dest");
        assert!((arrived_ball.velocity - DVec3::X * 100.0).length() < 1e-6);

        let arrived_cs = dest_handle
            .get_combat_state(ship_id)
            .await
            .unwrap()
            .expect("arrived combat state");
        assert!(arrived_cs.is_cloaked);
        assert_eq!(arrived_cs.gate_cloak_remaining_ticks, 60);
        assert_eq!(arrived_cs.undock_invulnerable_remaining_ticks, 60);
        assert!(arrived_cs.sync_invuln_with_undock_timer);
        assert!((arrived_cs.shield_hp - 150.0).abs() < 1e-6);
        assert!((arrived_cs.armor_hp - 120.0).abs() < 1e-6);
        assert!((arrived_cs.structure_hp - 90.0).abs() < 1e-6);
        assert!((arrived_cs.capacitor - 80.0).abs() < 1e-6);
        assert!(
            crate::services::space_destiny::check_session_change_timer_active(
                &ctx,
                cid,
                now_u,
                Some((dest_sys, dest_tick)),
            )
            .is_some(),
            "shipcaster jump must record session-change timer"
        );

        let moved_pos = DVec3::new(33_000.0, -250.0, 400.0);
        assert!(dest_handle.set_ball_position(ship_id, moved_pos).await);
        dest_handle
            .modify_combat_state(ship_id, |cs| {
                cs.is_cloaked = false;
                cs.gate_cloak_remaining_ticks = 0;
                cs.undock_invulnerable_remaining_ticks = 0;
            })
            .await
            .unwrap()
            .expect("dest combat state");
        dest_handle
            .apply_true_damage(ship_id, 15.0)
            .await
            .expect("apply_true_damage");
        assert_eq!(
            dest_handle
                .flush_condition_checkpoint()
                .await
                .expect("flush_condition_checkpoint"),
            1,
            "post-shipcaster ConditionCheckpoint must persist with synced epoch (`T2E-R3-01`)"
        );
        ctx.db.with_snapshot(|snap| {
            let p = snap
                .item_positions
                .get(&ship_id)
                .copied()
                .expect("item_positions persisted after shipcaster jump");
            assert!((DVec3::from_array(p) - moved_pos).length() < 1e-3);
            let hp = snap
                .sp6_lifecycle
                .ship_combat_hp
                .get(&ship_id)
                .expect("ship_combat_hp updated after shipcaster jump");
            assert!((hp.shield_hp - 135.0).abs() < 1e-3);
        });
    }

    #[tokio::test]
    async fn execute_shipcaster_jump_preserves_active_overloaded_module_damage_and_fit_cache() {
        let ctx = ServerContext::new();
        let origin_sys = 30_000_142_u32;
        let dest_sys = 30_000_144_u32;
        let cid = 90_881_222_i64;
        let ship_id = 91_881_222_i64;
        let mod_id = 92_881_222_i64;
        let corp_id = 98_000_001_i64;
        let now_u = ctx.now_unix();
        let mod_type_id = 440_u32;

        ctx.db.with_mut_snapshot(|snap| {
            let mut ch = CharacterRecord::default();
            ch.character_id = cid;
            ch.name = "OverheatedRookie".into();
            ch.corporation_id = corp_id;
            ch.solar_system_id = origin_sys;
            ch.active_ship_id = ship_id;
            ch.create_date_unix = now_u.saturating_sub(3_600);
            ch.station_id = None;
            ch.structure_id = None;
            snap.characters.insert(cid, ch);

            let mut ship = InventoryItemRecord::default();
            ship.item_id = ship_id;
            ship.type_id = 587;
            ship.owner_id = cid;
            ship.location_id = i64::from(origin_sys);
            ship.flag = InventoryFlag::None;
            ship.quantity = 1;
            ship.singleton = true;
            snap.items.insert(ship_id, ship);

            let mut module = InventoryItemRecord::default();
            module.item_id = mod_id;
            module.type_id = mod_type_id;
            module.owner_id = cid;
            module.location_id = ship_id;
            module.flag = InventoryFlag::MedSlot0;
            module.quantity = 1;
            module.singleton = true;
            snap.items.insert(mod_id, module);

            for tid in [587_u32, mod_type_id] {
                for (sk_tid, _) in ctx.sde.required_skills_for_type(tid) {
                    snap.skills.entry(cid).or_default().insert(
                        sk_tid,
                        eve_db::TrainedSkillRecord {
                            type_id: sk_tid,
                            level: 5,
                            skill_points: 256_000.0,
                        },
                    );
                }
            }

            // Active AND overloaded so departure deactivates and preserves `saved_heat` + `ship_fittings`.
            snap.module_online_states.insert(mod_id, true);
            snap.module_active_states.insert(mod_id);
            snap.ship_fit_inputs.insert(
                ship_id,
                eve_db::ShipFitInputsRecord {
                    active_stance_type_id: None,
                    last_stance_switch_ms: None,
                    overloaded_modules: std::collections::BTreeSet::from([mod_id]),
                },
            );
        });

        // Prime `ctx.ship_fittings` with live rack heat and live module heat damage (`T2E-P1-01`).
        let mut primed = ctx.fit(cid, ship_id).expect("prime ship fit");
        primed.rack_heat = [12.5, 4.25, 0.0];
        primed
            .modules
            .get_mut(&mod_id)
            .expect("fitted module present in primed fit")
            .damage_hp = 37.5;
        ctx.ship_fittings
            .write()
            .unwrap()
            .insert(ship_id, primed);
        assert!(ctx.db.with_snapshot(|s| s.module_active_states.contains(&mod_id)));

        let origin_handle = ctx.systems.get_or_spawn(origin_sys);
        origin_handle
            .spawn_ball_with_meta(
                Ball {
                    id: ship_id,
                    type_id: 587,
                    position: DVec3::new(1_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 0.5,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_ship_combat_state(cid, ship_id, 587),
                ctx.build_ship_slim_meta(cid, 587),
            )
            .await
            .expect("spawn origin ship");

        let mut sess = SessionState::new(7702);
        sess.user_id = Some(7702);
        sess.character_id = Some(cid);
        sess.corporation_id = Some(corp_id);
        sess.solar_system_id = Some(origin_sys);
        sess.ship_id = Some(ship_id);

        execute_shipcaster_jump(
            &ctx,
            &mut sess,
            cid,
            ship_id,
            origin_sys,
            dest_sys,
            DVec3::new(5_000.0, 0.0, 0.0),
            DVec3::X,
        )
        .await
        .expect("shipcaster jump succeeds with active/overloaded module");

        ctx.db.with_snapshot(|snap| {
            let db_dmg = snap.items[&mod_id]
                .dynamic_attributes
                .get(&eve_sde::attr_ids::DAMAGE)
                .copied();
            assert_eq!(db_dmg, Some(37.5), "DB DAMAGE attr must persist saved_heat");
            assert!(!snap.module_active_states.contains(&mod_id));
        });
        let cached_fit = ctx
            .ship_fittings
            .read()
            .unwrap()
            .get(&ship_id)
            .cloned()
            .expect("ship_fittings cache populated after jump");
        assert_eq!(cached_fit.rack_heat, [12.5, 4.25, 0.0]);
        assert_eq!(
            cached_fit.modules.get(&mod_id).map(|m| m.damage_hp),
            Some(37.5),
            "cached ctx.ship_fittings[ship_id].modules[mod_id].damage_hp must not be wiped (`T2E-P1-01`)"
        );
    }

    #[tokio::test]
    async fn execute_shipcaster_jump_spawn_failure_preserves_zarzakh_lock_heat_damage_fit_cache_and_ship_ball() {
        let ctx = ServerContext::new();
        let origin_sys = 30_100_000_u32; // Zarzakh
        let dest_sys = 30_000_144_u32; // Perimeter
        let cid = 90_881_333_i64;
        let ship_id = 91_881_333_i64;
        let mod_id = 92_881_333_i64;
        let corp_id = 98_000_001_i64;
        let now_u = ctx.now_unix();
        let mod_type_id = 440_u32;
        let lock_expiry = now_u + 3_600;

        ctx.db.with_mut_snapshot(|snap| {
            let mut ch = CharacterRecord::default();
            ch.character_id = cid;
            ch.name = "ZarzakhRollbackPilot".into();
            ch.corporation_id = corp_id;
            ch.solar_system_id = origin_sys;
            ch.active_ship_id = ship_id;
            ch.create_date_unix = now_u.saturating_sub(3_600);
            snap.characters.insert(cid, ch);

            let mut ship = InventoryItemRecord::default();
            ship.item_id = ship_id;
            ship.type_id = 587;
            ship.owner_id = cid;
            ship.location_id = i64::from(origin_sys);
            ship.flag = InventoryFlag::None;
            ship.quantity = 1;
            ship.singleton = true;
            snap.items.insert(ship_id, ship);

            let mut module = InventoryItemRecord::default();
            module.item_id = mod_id;
            module.type_id = mod_type_id;
            module.owner_id = cid;
            module.location_id = ship_id;
            module.flag = InventoryFlag::MedSlot0;
            module.quantity = 1;
            module.singleton = true;
            snap.items.insert(mod_id, module);

            for tid in [587_u32, mod_type_id] {
                for (sk_tid, _) in ctx.sde.required_skills_for_type(tid) {
                    snap.skills.entry(cid).or_default().insert(
                        sk_tid,
                        eve_db::TrainedSkillRecord {
                            type_id: sk_tid,
                            level: 5,
                            skill_points: 256_000.0,
                        },
                    );
                }
            }

            snap.zarzakh_gate_locks.insert(cid, (50_001_234, lock_expiry));
            snap.module_online_states.insert(mod_id, true);
            snap.module_active_states.insert(mod_id);
            snap.ship_fit_inputs.insert(
                ship_id,
                eve_db::ShipFitInputsRecord {
                    active_stance_type_id: None,
                    last_stance_switch_ms: None,
                    overloaded_modules: std::collections::BTreeSet::from([mod_id]),
                },
            );
            snap.sp6_lifecycle.ship_combat_hp.insert(
                -ship_id,
                eve_db::PersistedCombatHp {
                    shield_hp: 0.0,
                    armor_hp: f64::from(origin_sys),
                    hull_hp: 2.0,
                },
            );
        });

        let mut primed = ctx.fit(cid, ship_id).expect("prime ship fit");
        primed.rack_heat = [12.5, 4.25, 0.0];
        primed
            .modules
            .get_mut(&mod_id)
            .expect("fitted module present in primed fit")
            .damage_hp = 37.5;
        ctx.ship_fittings.write().unwrap().insert(ship_id, primed);
        assert!(ctx.db.with_snapshot(|s| s.module_active_states.contains(&mod_id)));

        let origin_handle = ctx.systems.get_or_spawn(origin_sys);
        origin_handle
            .spawn_ball_with_meta(
                Ball {
                    id: ship_id,
                    type_id: 587,
                    position: DVec3::new(1_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 0.5,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_ship_combat_state(cid, ship_id, 587),
                ctx.build_ship_slim_meta(cid, 587),
            )
            .await
            .expect("spawn origin ship");

        // Pre-wake destination and fail the very first command (`SpawnBallWithMeta`).
        let dest_handle = ctx
            .systems
            .wake_for_session_arrival_async(
                dest_sys,
                crate::system_actor::SessionArrivalWakeReason::Jump,
            )
            .await
            .expect("wake dest");
        dest_handle
            .panic_next_command_for_test
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let mut sess = SessionState::new(7703);
        sess.user_id = Some(7703);
        sess.character_id = Some(cid);
        sess.corporation_id = Some(corp_id);
        sess.solar_system_id = Some(origin_sys);
        sess.ship_id = Some(ship_id);

        let err = execute_shipcaster_jump(
            &ctx,
            &mut sess,
            cid,
            ship_id,
            origin_sys,
            dest_sys,
            DVec3::new(5_000.0, 0.0, 0.0),
            DVec3::X,
        )
        .await
        .expect_err("spawn_arrival_ship_ball failure must fail execute_shipcaster_jump and roll back");
        assert_eq!(err.code, "SystemTransitionFailed");
        assert_eq!(sess.solar_system_id, Some(origin_sys));

        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.characters[&cid].solar_system_id, origin_sys);
            assert_eq!(snap.items[&ship_id].location_id, i64::from(origin_sys));
            assert_eq!(
                snap.zarzakh_gate_locks.get(&cid).copied(),
                Some((50_001_234, lock_expiry)),
                "Zarzakh gate lock must remain intact when destination spawn fails (`T2E-R2-02`)"
            );
            let meta = snap
                .sp6_lifecycle
                .ship_combat_hp
                .get(&(-ship_id))
                .expect("ship_combat_hp meta must remain at origin_sys");
            assert_eq!(meta.armor_hp.round() as u32, origin_sys);
            assert!(snap.module_active_states.contains(&mod_id));
            assert!(snap.ship_fit_inputs[&ship_id]
                .overloaded_modules
                .contains(&mod_id));
            assert!(
                snap.items[&mod_id]
                    .dynamic_attributes
                    .get(&eve_sde::attr_ids::DAMAGE)
                    .is_none(),
                "pre-spawn DB DAMAGE write must roll back on spawn failure"
            );
        });
        let restored_fit = ctx
            .ship_fittings
            .read()
            .unwrap()
            .get(&ship_id)
            .cloned()
            .expect("ship_fittings cache restored");
        assert_eq!(restored_fit.rack_heat, [12.5, 4.25, 0.0]);
        assert_eq!(
            restored_fit.modules.get(&mod_id).map(|m| m.damage_hp),
            Some(37.5)
        );
        let (_, restored_ball) = origin_handle
            .get_ball_snapshot(ship_id)
            .await
            .expect("origin query")
            .expect("origin ship ball must be restored on spawn failure");
        assert!((restored_ball.position - DVec3::new(1_000.0, 0.0, 0.0)).length() < 1e-6);
    }
}


