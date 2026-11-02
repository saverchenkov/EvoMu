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
    if let Some(pws) = pending_notifications {
        let now_ft = db.clock().now_blueos_filetime_ticks();
        let mut target_systems = vec![(pad_sys, ball_id)];
        if let Some((war_hq_sys, launcher_id)) = launcher_opt {
            target_systems.push((war_hq_sys, launcher_id));
        }
        for (sys_id, item_id) in target_systems {
            let recipients: Vec<(i64, u64)> = db.with_snapshot(|snap| {
                snap.characters
                    .values()
                    .filter(|c| c.solar_system_id == sys_id)
                    .map(|c| (c.character_id, c.user_id))
                    .collect()
            });
            for (cid, uid) in recipients {
                let (packet, marshaled_packet) =
                    crate::router::ServerContext::build_macho_notification_packet(
                        cid,
                        Some(uid),
                        "OnShipcasterTargetStateChanged",
                        "solarsystemid",
                        vec![PyRep::Integer(item_id)],
                    );
                let note = PendingMachoNotification {
                    recipient_char_id: cid,
                    event: "OnShipcasterTargetStateChanged".into(),
                    id_type: "solarsystemid".into(),
                    args: vec![PyRep::Integer(item_id)],
                    packet,
                    marshaled_packet,
                    created_filetime: now_ft,
                };
                if let Ok(mut guard) = pws.write() {
                    guard.entry(cid).or_default().push(note.clone());
                }
                let _ = notify_tx.send((cid, note));
            }
        }
    }
}
