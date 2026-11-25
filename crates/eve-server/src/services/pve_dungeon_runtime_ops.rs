//! Child helpers for [`super::pve_dungeon_runtime`] keeping module size bounded
//! while preserving exact deadspace & special-site warp target resolution.

#![forbid(unsafe_code)]

use super::*;

pub(super) fn resolve_fw_or_special_warp_target(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    target_id: i64,
    target_str_opt: Option<&str>,
) -> Option<DVec3> {
    let fw_pos = ctx.db.with_snapshot(|snap| {
        let has_fw = snap
            .fw_runtime
            .fw_complexes
            .iter()
            .any(|(&gid, (csys, _, _, _, done))| gid == target_id && *csys == sys_id && !*done)
            || snap
                .insurgency_runtime
                .insurgency_instances
                .iter()
                .any(|s| s.instance_id == target_id && s.solar_system_id == sys_id && !s.completed);
        has_fw
            .then(|| {
                snap.system_states
                    .get(&sys_id)
                    .and_then(|st| st.fw_complex_position)
                    .map(DVec3::from_array)
            })
            .flatten()
    });
    if fw_pos.is_some() {
        return fw_pos;
    }
    crate::services::pve_special_sites::read_pve_special_reg(ctx, |reg| {
        if let Some(hf) = reg.homefront_sites.get(&sys_id).and_then(|v| {
            v.iter()
                .find(|s| !s.completed && (s.instance_id == target_id || i64::from(s.dungeon_id) == target_id))
        }) {
            return Some(DVec3::new(
                10_000.0 + ((hf.instance_id % 100) as f64) * 5_000.0,
                ((hf.instance_id % 10) as f64) * 1_000.0,
                0.0,
            ));
        }
        if let Some(tr) = reg.triglavian_sites.get(&sys_id).and_then(|v| {
            v.iter()
                .find(|s| !s.completed && (s.instance_id == target_id || i64::from(s.dungeon_id) == target_id))
        }) {
            return Some(DVec3::new(
                10_000.0 + ((tr.instance_id % 100) as f64) * 2_500.0,
                0.0,
                0.0,
            ));
        }
        for inc in reg
            .incursions
            .values()
            .filter(|i| i.state != "Ended" && i.state != "Withdrawing")
        {
            if let Some(s) = inc.sites.iter().find(|s| {
                s.solar_system_id == sys_id
                    && (target_str_opt == Some(s.site_id.as_str())
                        || i64::from(s.dungeon_id) == target_id)
            }) {
                return Some(DVec3::from_array(s.position));
            }
        }
        if reg.drifter_hives.get(&sys_id).is_some_and(|h| {
            !h.cleared
                && ((target_id > 0
                    && (i64::from(h.dungeon_id) == target_id
                        || i64::from(h.solar_system_id) == target_id
                        || h.rooms.first().is_some_and(|r| {
                            r.beacon_ball_id > 0 && r.beacon_ball_id == target_id
                        })))
                    || target_str_opt == Some(h.site_id.as_str()))
        }) {
            return Some(DVec3::new(10_000.0, 0.0, 0.0));
        }
        None
    })
}

pub(super) fn find_active_dungeon_id_at_position(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    pos: [f64; 3],
) -> Option<u32> {
    let runtime_did = read_dungeon_runtime_store(ctx, |store| {
        store.instances.values().find_map(|inst| {
            if instance_is_finished(inst) || inst.solar_system_id != sys_id {
                return None;
            }
            let near_entry = inst
                .entry_gate_pos
                .is_some_and(|ep| dist3(pos, ep) <= 150_000.0);
            let near_room = inst
                .rooms
                .iter()
                .enumerate()
                .any(|(r_idx, r)| room_contains_pocket_pos(inst, r_idx, r, pos, 150_000.0));
            (near_entry || near_room).then_some(inst.dungeon_id)
        })
    });
    if runtime_did.is_some() {
        return runtime_did;
    }

    read_pve_exploration_reg(ctx, |reg| {
        reg.sites_by_system.get(&sys_id).and_then(|sites| {
            sites
                .iter()
                .find(|s| !s.cleared && dist3(pos, s.position) <= 150_000.0)
                .map(|s| s.dungeon_id)
        })
    })
}

pub(super) struct CompletedDungeonCleanup {
    pub(super) dungeon_id: u32,
    pub(super) recipient_char_id: i64,
    pub(super) balls_to_remove: Vec<i64>,
    pub(super) emit_completed: bool,
}

pub(super) fn apply_complete_dungeon_instance_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    instance_id: i64,
    char_id: i64,
    force_full_despawn: bool,
    now_unix: u64,
) -> Option<CompletedDungeonCleanup> {
    let was_despawned = snap
        .dungeon_runtime
        .instances
        .get(&instance_id)
        .is_none_or(|i| i.despawned);
    if was_despawned {
        return None;
    }
    let has_loot = !force_full_despawn
        && snap
            .dungeon_runtime
            .instances
            .get(&instance_id)
            .is_some_and(|i| has_unlooted_containers_in_snap(snap, i));

    let mut balls_to_remove = Vec::new();
    let mut container_cleanup_ids = Vec::new();
    let mut result = None;

    if let Some(inst) = snap.dungeon_runtime.instances.get_mut(&instance_id) {
        let was_completed = inst.completed;
        let was_failed = inst.failed;
        record_dungeon_actor(inst, char_id);
        if !was_failed {
            inst.completed = true;
            inst.completed_unix.get_or_insert(now_unix);
        }
        for obj in &mut inst.objectives {
            if !was_failed && obj.status != DungeonObjectiveStatus::Completed {
                obj.status = DungeonObjectiveStatus::Completed;
            }
        }
        if let Some(eg) = inst.entry_gate_ball_id.take() {
            balls_to_remove.push(eg);
        }
        for r in &mut inst.rooms {
            balls_to_remove.extend(collect_all_room_gate_ids(r));
            r.gate_ball_id = None;
            for gl in &mut r.gate_links {
                gl.gate_ball_id = None;
            }
            for grp in &mut r.npc_groups {
                balls_to_remove.extend(grp.npc_ball_ids.drain(..));
                grp.dead_npc_ball_ids.clear();
                grp.npc_ball_meta.clear();
                grp.npc_ball_tags.clear();
                grp.npc_ball_names.clear();
                grp.npc_ball_bounties.clear();
                grp.cleared = true;
            }
            r.cleared = true;
            for obj in &mut r.objects {
                let keep_for_loot =
                    has_loot && obj.is_container && obj.spawned && !obj.destroyed;
                if !keep_for_loot && obj.ball_id > 0 {
                    if obj.is_container {
                        container_cleanup_ids.push(obj.ball_id);
                    }
                    balls_to_remove.push(obj.ball_id);
                    obj.ball_id = 0;
                    obj.spawned = false;
                }
            }
        }
        if has_loot {
            inst.despawned = false;
            let grace_until = now_unix.saturating_add(DUNGEON_COMPLETION_LOOT_GRACE_SECS);
            inst.loot_grace_until_unix.get_or_insert(grace_until);
            inst.despawn_after_unix.get_or_insert(grace_until);
        } else {
            inst.despawned = true;
            inst.loot_grace_until_unix = None;
            inst.despawn_after_unix = Some(now_unix);
        }
        let recipient = resolve_dungeon_recipient(inst, char_id);
        let emit_completed = !was_completed && !was_failed;
        result = Some(CompletedDungeonCleanup {
            dungeon_id: inst.dungeon_id,
            recipient_char_id: recipient,
            balls_to_remove,
            emit_completed,
        });
    }

    cleanup_instance_containers_in_snap(snap, &container_cleanup_ids);
    result
}

#[allow(clippy::too_many_arguments)]
pub(super) fn finalize_completed_dungeon_effects_sync(
    db: &std::sync::Arc<eve_db::WorldRepository>,
    pending_notifications: Option<
        &dyn AnyRwLock<
            HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>,
        >,
    >,
    notify_tx: Option<
        &tokio::sync::broadcast::Sender<(
            i64,
            crate::router::cache_notifications::PendingMachoNotification,
        )>,
    >,
    sys_id: SolarSystemId,
    dungeon_id: u32,
    recipient_char_id: i64,
    emit_completed: bool,
    now_unix: u64,
) {
    if !emit_completed {
        return;
    }
    let mut cleared_sig_ids = Vec::new();
    crate::services::pve_exploration::with_pve_exploration_reg_db(db, now_unix, |reg| {
        if let Some(sites) = reg.sites_by_system.get_mut(&sys_id) {
            for s in sites.iter_mut() {
                if !s.cleared && s.dungeon_id == dungeon_id && s.site_category == "Deadspace" {
                    s.cleared = true;
                    cleared_sig_ids.push(s.site_id.clone());
                }
            }
        }
    });
    if !cleared_sig_ids.is_empty() {
        let mut collapsed_now = Vec::new();
        db.with_mut_snapshot(|snap| {
            if let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id) {
                for sig in sigs.iter_mut() {
                    if !sig.collapsed && cleared_sig_ids.contains(&sig.signature_id) {
                        sig.collapsed = true;
                        collapsed_now.push(sig.signature_id.clone());
                    }
                }
            }
        });
        if let (Some(pend), Some(tx)) = (pending_notifications, notify_tx) {
            for sig_id in &collapsed_now {
                crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_site_removed_with_db(
                    db, pend, tx, sig_id,
                );
            }
        }
    }
    if recipient_char_id > 0 {
        crate::services::notify_emitters_ess_bounties::push_char_notification_with_db(
            db,
            pending_notifications,
            notify_tx,
            recipient_char_id,
            "OnDungeonCompleted",
            vec![PyRep::Integer(i64::from(dungeon_id))],
        );
    }
}

fn collect_due_loot_grace_instances(ctx: &ServerContext, now_unix: u64) -> Vec<(SolarSystemId, i64)> {
    if !has_dungeon_runtime_store(ctx) {
        return Vec::new();
    }
    read_dungeon_runtime_store(ctx, |store| {
        store
            .instances
            .values()
            .filter(|inst| {
                (inst.completed || inst.failed)
                    && !inst.despawned
                    && inst
                        .loot_grace_until_unix
                        .or(inst.despawn_after_unix)
                        .is_some_and(|due| now_unix >= due)
            })
            .map(|inst| (inst.solar_system_id, inst.instance_id))
            .collect()
    })
}

/// Synchronously finalizes all completed/failed dungeon instances whose loot-grace deadline
/// has elapsed (called from `sp6_lifecycle::cluster_tick::on_advance_secs_internal`).
pub(crate) fn expire_due_dungeon_loot_grace_sync(ctx: &ServerContext) -> usize {
    let now_unix = ctx.db.clock().now_unix_secs();
    let due = collect_due_loot_grace_instances(ctx, now_unix);
    if due.is_empty() {
        return 0;
    }
    let mut swept = 0usize;
    for (sys_id, inst_id) in due {
        let cleanup = ctx.db.with_mut_snapshot(|snap| {
            apply_complete_dungeon_instance_snap(snap, inst_id, 0, true, now_unix)
        });
        if let Some(done) = cleanup {
            swept += 1;
            if let Some(sys_handle) = ctx.systems.get_handle(sys_id) {
                for bid in done.balls_to_remove {
                    sys_handle.try_remove_ball(bid);
                }
            }
            finalize_completed_dungeon_effects_sync(
                &ctx.db,
                Some(&ctx.pending_notifications),
                Some(&ctx.notify_tx),
                sys_id,
                done.dungeon_id,
                done.recipient_char_id,
                done.emit_completed,
                now_unix,
            );
        }
    }
    swept
}

/// Asynchronously finalizes all completed/failed dungeon instances whose loot-grace deadline
/// has elapsed across all solar systems (called from `tick_background_cluster_with_step_inner`).
pub(crate) async fn expire_all_due_dungeon_loot_grace(ctx: &ServerContext) -> usize {
    let now_unix = ctx.db.clock().now_unix_secs();
    let due = collect_due_loot_grace_instances(ctx, now_unix);
    let count = due.len();
    for (sys_id, inst_id) in due {
        complete_dungeon_instance_core(
            &ctx.systems,
            &ctx.db,
            Some(&ctx.pending_notifications),
            Some(&ctx.notify_tx),
            sys_id,
            inst_id,
            0,
            true,
        )
        .await;
    }
    count
}

#[cfg(test)]
#[path = "pve_dungeon_runtime_ops_edges_tests.rs"]
mod pve_dungeon_runtime_ops_edges_tests;
