use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

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
        if st_cw.is_weapon_timer_active(now)
            || st_cw.is_pvp_active(now)
            || st_cw.criminal_flag_until_unix > now
        {
            return Err(rpc_err(
                "WeaponsTimerActive",
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
    pending_notifications: Option<&std::sync::RwLock<std::collections::HashMap<i64, Vec<PendingMachoNotification>>>>,
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
    let mut queues = pws.write().unwrap_or_else(|e| e.into_inner());
    for (cid, uid) in recipients {
        let args = vec![PyRep::Integer(item_id)];
        let (packet, marshaled_packet) = ServerContext::build_macho_notification_packet(
            cid,
            uid,
            "OnShipcasterTargetStateChanged",
            "solarsystemid",
            args.clone(),
        );
        let note = PendingMachoNotification {
            seq_id: crate::router::cache_notifications::next_notification_seq_id(),
            recipient_char_id: cid,
            event: "OnShipcasterTargetStateChanged".into(),
            id_type: "solarsystemid".into(),
            args,
            packet,
            marshaled_packet,
            created_filetime: now_ft,
        };
        let q = queues.entry(cid).or_default();
        if q.len() >= crate::router::cache_notifications::MAX_PENDING_NOTIFICATIONS_PER_CHAR {
            q.remove(0);
        }
        q.push(note.clone());
        let _ = notify_tx.send((cid, note));
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
    misc: &std::sync::Arc<std::sync::Mutex<crate::services::cluster_macho_misc_services::ClusterMiscRuntimeState>>,
    systems: &std::sync::Arc<crate::system_actor::SystemRegistry>,
    db: &std::sync::Arc<eve_db::WorldRepository>,
    sde: &eve_sde::SdeRegistry,
    pending_notifications: Option<&std::sync::RwLock<std::collections::HashMap<i64, Vec<PendingMachoNotification>>>>,
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
            rpc_err(
                "ShipNotInSpace",
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
        return Err(rpc_err(
            "ShipNotInSpace",
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
            return Err(rpc_err(
                "WeaponsTimerActive",
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
    let (_, ship_ball, pre_cs) =
        prevalidate_shipcaster_jump(ctx, char_id, ship_id, origin_sys).await?;
    let old_handle = ctx.systems.get_or_spawn(origin_sys);

    let updated_ch = ctx
        .db
        .set_character_solar_system(char_id, dest_sys, &ctx.sde)
        .map_err(|e| rpc_err("SystemTransitionFailed", &e.to_string()))?;
    crate::sp6_lifecycle::abort_self_destruct_for_ship(ctx, ship_id);

    let saved_heat = ctx
        .ship_fittings
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&ship_id)
        .map(|f| {
            let dmg: Vec<(i64, f64)> = f
                .modules
                .iter()
                .filter(|(_, m)| m.damage_hp.is_finite() && m.damage_hp > 0.0)
                .map(|(&mid, m)| (mid, m.damage_hp))
                .collect();
            (f.rack_heat, dmg)
        });

    let cleared_zarzakh_gate = ctx.db.with_mut_snapshot(|snap| {
        if let Some((_, ref dmg_list)) = saved_heat {
            for &(mid, dhp) in dmg_list {
                if let Some(it) = snap.items.get_mut(&mid) {
                    it.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, dhp);
                }
            }
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = dest_sys as i64;
        }
        if origin_sys == 30_100_000 {
            snap.zarzakh_gate_locks.remove(&char_id).map(|(gid, _)| gid)
        } else {
            None
        }
    });
    if let Some(locked_gate_id) = cleared_zarzakh_gate {
        crate::services::quasar_sov_services::publish_stargate_lock_deactivated_notice(
            &ctx.quasar_gateway,
            char_id,
            30_100_000,
            locked_gate_id,
            now_unix,
        );
    }

    let live_cs = old_handle
        .get_combat_state(ship_id)
        .await
        .ok()
        .flatten()
        .or(pre_cs);
    if let Some(ref prev_cs) = live_cs {
        if prev_cs.structure_hp.is_finite() && prev_cs.structure_hp > 0.0 {
            crate::sp6_lifecycle::persist_ship_combat_hp_in_db(&ctx.db, ship_id, prev_cs);
        }
    }

    crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, origin_sys);
    crate::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(
        ctx, char_id, ship_id,
    );
    if let Some((rack_heat, _)) = saved_heat {
        if rack_heat.iter().any(|&h| h.is_finite() && h > 0.0) {
            if let Ok(mut fit) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                fit.rack_heat = rack_heat;
                ctx.store_ship_fitting(ship_id, fit);
            }
        }
    }
    let existing_ball = old_handle.remove_ball(ship_id).await;
    ctx.sync_character_local_chat(char_id, Some(origin_sys), dest_sys);

    let new_handle = ctx.systems.get_or_spawn(dest_sys);
    let mut new_ball = existing_ball.unwrap_or(ship_ball);
    new_ball.position = arrival_pos;
    new_ball.velocity = launch_vector * 100.0;
    new_ball.speed_fraction = 0.0;
    new_ball.mode = MovementMode::Stop;

    let mut combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
    if let Some(prev_cs) = live_cs {
        if prev_cs.shield_hp.is_finite() {
            combat_state.shield_hp = prev_cs.shield_hp.clamp(0.0, combat_state.max_shield_hp.max(0.0));
        }
        if prev_cs.armor_hp.is_finite() {
            combat_state.armor_hp = prev_cs.armor_hp.clamp(0.0, combat_state.max_armor_hp.max(0.0));
        }
        if prev_cs.structure_hp.is_finite() && prev_cs.structure_hp > 0.0 {
            combat_state.structure_hp = prev_cs
                .structure_hp
                .clamp(1e-6, combat_state.max_structure_hp.max(1e-6));
        }
        if prev_cs.capacitor.is_finite() {
            combat_state.capacitor = prev_cs.capacitor.clamp(0.0, combat_state.max_capacitor.max(0.0));
        }
    }
    combat_state.is_cloaked = true;
    combat_state.gate_cloak_remaining_ticks = 60;
    combat_state.undock_invulnerable_remaining_ticks = 60;
    combat_state.sync_invuln_with_undock_timer = true;
    let mut slim_meta = ctx.build_ship_slim_meta(char_id, new_ball.type_id);
    slim_meta.is_cloaked = true;

    let new_tick = new_handle
        .spawn_ball_with_meta(new_ball, combat_state, slim_meta)
        .await
        .unwrap_or(0);
    apply_shipcaster_arrival_fx(&new_handle, ship_id).await;

    crate::services::space_destiny::record_session_change_timer(
        ctx,
        char_id,
        now_unix,
        dest_sys,
        new_tick,
        "shipcaster",
    );

    let _ = updated_ch;
    let _ = ctx.execute_session_transition_sync(
        Some(session),
        crate::router::SessionTransitionSpec::for_kind(
            char_id,
            crate::router::TransitionKind::JumpSystem { solar_system_id: dest_sys },
            "shipcaster",
        )
        .with_apply_db(false)
        .with_cleanup_departing_space(false)
        .with_sync_local_chat(false)
        .with_emit_on_session_changed(false),
    );
    crate::services::map_service::record_solar_system_jump(ctx, char_id, dest_sys);
    let _ = ctx.bind_object(session, "beyonce", dest_sys as i64);
    let _ = new_handle.push_set_state(ship_id).await;

    Ok(())
}

#[cfg(test)] #[path = "shipcaster_ops_starter_tests.rs"] mod shipcaster_ops_starter_tests;
#[cfg(test)] #[path = "shipcaster_ops_starter_edges_tests.rs"] mod shipcaster_ops_starter_edges_tests;


