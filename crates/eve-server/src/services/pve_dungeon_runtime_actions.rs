//! Trigger evaluation, multi-action execution, completion/failure lifecycle, and
//! post-tx effect flushing for [`super::pve_dungeon_runtime`] (`#1495` Track 2a-2).

#![forbid(unsafe_code)]

use super::{
    insert_container_and_loot_in_snap, instance_is_finished, plan_npc_group_spawns_in_snap,
    resolve_room_recipient, spawn_gate_ball, spawn_gate_ball_sync, spawn_object_ball,
    spawn_object_ball_sync, wake_dungeon_system_handle, NpcSpawnOrder, DEFAULT_CONTAINER_TYPE_ID,
};
use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_npc_ai::build_specialized_npc_spawn_item;
use crate::services::combat_crimewatch::{
    with_mut_combat_runtime_with_locks, CombatRuntimeState, ExtendedNpcEntity,
};
use crate::services::pve_expeditions::{create_escalation_expedition_with_options_core, expedition_sig_id};
use eve_db::lock_rank::AnyRwLock;
use eve_db::service_domains::pve_sites::{
    DungeonObjective, DungeonObjectiveStatus, DungeonRuntimeInstance, DungeonRuntimeObject,
    DungeonTriggerAction,
};
use eve_db::types::{IskCents, StandingTransactionRecord, WalletJournalEntry};
use eve_db::WorldStateSnapshot;
use eve_proto::PyRep;
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(crate) struct ObjectSpawnOrder {
    pub(crate) ball_id: i64,
    pub(crate) type_id: u32,
    pub(crate) position: [f64; 3],
    pub(crate) radius: f64,
    pub(crate) destructible: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct DungeonRuntimeDeps<'a> {
    pub(crate) systems: &'a Arc<crate::system_actor::SystemRegistry>,
    pub(crate) db: &'a Arc<eve_db::WorldRepository>,
    pub(crate) sde: Option<&'a eve_sde::SdeRegistry>,
    pub(crate) sde_arc: Option<&'a Arc<eve_sde::SdeRegistry>>,
    pub(crate) combat_ephemeral: Option<&'a dyn AnyRwLock<CombatRuntimeState>>,
    pub(crate) pending_notifications: Option<
        &'a dyn AnyRwLock<HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>>,
    >,
    pub(crate) notify_tx:
        Option<&'a tokio::sync::broadcast::Sender<(i64, crate::router::cache_notifications::PendingMachoNotification)>>,
    pub(crate) ctx: Option<&'a ServerContext>,
}

impl<'a> DungeonRuntimeDeps<'a> {
    pub(crate) fn from_ctx(ctx: &'a ServerContext) -> Self {
        Self {
            systems: &ctx.systems, db: &ctx.db, sde: Some(&ctx.sde), sde_arc: Some(&ctx.sde),
            combat_ephemeral: Some(&*ctx.combat_ephemeral),
            pending_notifications: Some(&ctx.pending_notifications), notify_tx: Some(&ctx.notify_tx),
            ctx: Some(ctx),
        }
    }
    pub(crate) fn from_parts(
        systems: &'a Arc<crate::system_actor::SystemRegistry>, db: &'a Arc<eve_db::WorldRepository>,
        sde: Option<&'a eve_sde::SdeRegistry>,
        pending_notifications: Option<&'a dyn AnyRwLock<HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>>>,
        notify_tx: Option<&'a tokio::sync::broadcast::Sender<(i64, crate::router::cache_notifications::PendingMachoNotification)>>,
    ) -> Self {
        Self { systems, db, sde, sde_arc: None, combat_ephemeral: None, pending_notifications, notify_tx, ctx: None }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ProgressionNotice {
    TaskEvents(i64, Vec<(i64, i64)>),
    AdvancedOnly(i64, i64, i64),
}

#[derive(Default)]
pub(crate) struct TriggerTxEffects {
    pub(crate) despawn_npc_ids: Vec<i64>,
    pub(crate) despawn_other_balls: Vec<i64>,
    pub(crate) spawn_gates: Vec<(i64, [f64; 3], Option<u32>)>,
    pub(crate) spawn_objects: Vec<ObjectSpawnOrder>,
    pub(crate) spawn_npcs: Vec<NpcSpawnOrder>,
    pub(crate) notify_actions: Vec<(i64, u32, Vec<i64>, Vec<DungeonTriggerAction>)>,
    pub(crate) progression_notices: Vec<ProgressionNotice>,
    pub(crate) completed_sites: Vec<(i64, u32, i64, bool)>,
    pub(crate) failed_notices: Vec<(i64, u32, i64)>,
    pub(crate) escalation_requests: Vec<(i64, u32, Option<u32>, u16, Option<String>, [f64; 3], i64)>,
    pub(crate) locked_rooms: HashSet<usize>,
    pub(crate) affected_clear_rooms: Vec<usize>,
    pub(crate) complete_now: bool,
    pub(crate) fail_reason: Option<String>,
}

impl TriggerTxEffects {
    /// Strips any ball ID queued in `despawn_*` from `spawn_*` before flushing (`OWN-R5-02`).
    pub(crate) fn cancel_dead_spawns(&mut self) {
        if self.despawn_npc_ids.is_empty() && self.despawn_other_balls.is_empty() { return; }
        let dead: HashSet<i64> = self.despawn_npc_ids.iter().copied().chain(self.despawn_other_balls.iter().copied()).collect();
        self.spawn_npcs.retain(|o| !dead.contains(&o.ball_id));
        self.spawn_objects.retain(|o| !dead.contains(&o.ball_id));
        self.spawn_gates.retain(|(gid, _, _)| !dead.contains(gid));
    }
}

pub(crate) type TriggerBatch = Vec<(u32, Option<usize>, bool, i64, Vec<DungeonTriggerAction>)>;

fn deterministic_roll_bps(seed_a: i64, seed_b: u64) -> u16 {
    let mut x = (seed_a as u64) ^ seed_b.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    ((x ^ (x >> 31)) % 10_000) as u16
}

fn build_dungeon_npc_spawn_tuple(
    sde: &eve_sde::SdeRegistry,
    sys_id: SolarSystemId,
    order: NpcSpawnOrder,
) -> (
    eve_destiny::Ball,
    eve_destiny::CombatState,
    eve_destiny::SlimItemMeta,
    ExtendedNpcEntity,
) {
    let seed = order.ball_id.unsigned_abs() ^ (u64::from(order.type_id) << 16);
    let (ball, mut cs, mut meta, mut entity) = build_specialized_npc_spawn_item(
        sde,
        sys_id,
        order.ball_id,
        order.type_id,
        "",
        u32::from(order.group_id),
        DVec3::from_array(order.pos),
        seed,
        &mut |_| {},
    );
    if !order.destructible {
        cs.is_tethered = true;
    }
    if let Some(name) = order.custom_name {
        meta.name = Some(name);
    }
    if let Some(bounty_isk) = order.bounty_isk_override {
        entity.bounty_isk = bounty_isk.max(0) as f64;
    }
    (ball, cs, meta, entity)
}

fn record_spawned_dungeon_npc_entities(
    deps: DungeonRuntimeDeps<'_>,
    entities: Vec<(i64, ExtendedNpcEntity)>,
) {
    if let (Some(combat_eph), false) = (deps.combat_ephemeral, entities.is_empty()) {
        with_mut_combat_runtime_with_locks(deps.db, combat_eph, |rt| {
            for (bid, ent) in entities {
                rt.npc_entities.insert(bid, ent);
            }
        });
    }
}

pub(crate) async fn spawn_dungeon_npc_orders(
    deps: DungeonRuntimeDeps<'_>,
    sys_id: SolarSystemId,
    sys_handle: &crate::system_actor::SystemHandle,
    orders: Vec<NpcSpawnOrder>,
) {
    let Some(sde) = deps.sde else { return };
    let mut entities = Vec::with_capacity(orders.len());
    for order in orders {
        let ball_id = order.ball_id;
        let (ball, cs, meta, entity) = build_dungeon_npc_spawn_tuple(sde, sys_id, order);
        let _ = sys_handle.spawn_ball_with_meta(ball, cs, meta).await;
        entities.push((ball_id, entity));
    }
    record_spawned_dungeon_npc_entities(deps, entities);
}

pub(crate) fn spawn_dungeon_npc_orders_sync(
    deps: DungeonRuntimeDeps<'_>,
    sys_id: SolarSystemId,
    sys_handle: &crate::system_actor::SystemHandle,
    orders: Vec<NpcSpawnOrder>,
) {
    let Some(sde) = deps.sde else { return };
    let mut entities = Vec::with_capacity(orders.len());
    for order in orders {
        let ball_id = order.ball_id;
        let (ball, cs, meta, entity) = build_dungeon_npc_spawn_tuple(sde, sys_id, order);
        if sys_handle.spawn_ball_with_meta_sync(ball, cs, meta).is_ok() {
            entities.push((ball_id, entity));
        }
    }
    record_spawned_dungeon_npc_entities(deps, entities);
}

fn remove_despawned_dungeon_npc_entities(deps: DungeonRuntimeDeps<'_>, npc_ids: &[i64]) {
    if let (Some(combat_eph), false) = (deps.combat_ephemeral, npc_ids.is_empty()) {
        with_mut_combat_runtime_with_locks(deps.db, combat_eph, |rt| {
            for bid in npc_ids {
                rt.npc_entities.remove(bid);
            }
        });
    }
}

pub(crate) async fn despawn_dungeon_npc_ids(
    deps: DungeonRuntimeDeps<'_>, sys_handle_opt: Option<&crate::system_actor::SystemHandle>, npc_ids: &[i64],
) {
    if npc_ids.is_empty() { return; }
    if let Some(sh) = sys_handle_opt { for &bid in npc_ids { let _ = sh.remove_ball(bid).await; } }
    remove_despawned_dungeon_npc_entities(deps, npc_ids);
}

pub(crate) fn despawn_dungeon_npc_ids_sync(
    deps: DungeonRuntimeDeps<'_>, sys_handle_opt: Option<&crate::system_actor::SystemHandle>, npc_ids: &[i64],
) {
    if npc_ids.is_empty() { return; }
    if let Some(sh) = sys_handle_opt { for &bid in npc_ids { let _ = sh.remove_ball_sync(bid); } }
    remove_despawned_dungeon_npc_entities(deps, npc_ids);
}

pub(crate) fn finish_instance_entities_in_snap(
    snap: &mut WorldStateSnapshot, instance_id: i64, actor_char_id: i64, now_unix: u64,
    is_fail: bool, force_full_despawn: bool, effects: &mut TriggerTxEffects,
) {
    let Some(done) = super::ops::apply_finish_dungeon_instance_snap(
        snap, instance_id, actor_char_id, is_fail, force_full_despawn, now_unix,
    ) else { return; };
    effects.despawn_other_balls.extend(done.non_npc_balls_to_remove);
    effects.despawn_npc_ids.extend(done.npc_balls_to_remove);
    if done.emit_failed {
        effects.failed_notices.push((done.recipient_char_id, done.dungeon_id, instance_id));
    } else if done.emit_completed {
        effects.completed_sites.push((instance_id, done.dungeon_id, done.recipient_char_id, !done.is_mission));
        if done.is_mission {
            crate::services::agents_missions::agents_missions_dungeon::sync_mission_from_dungeon_in_snap(snap, instance_id);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn grant_reward_in_snap(
    snap: &mut WorldStateSnapshot, sde_opt: Option<&eve_sde::SdeRegistry>,
    did: u32, recip: i64, isk: i64, lp: i64, corp_id: u32, faction_id: u32, standing_milli: i32, now_unix: u64,
) {
    if recip <= 0 { return; }
    let dungeon_faction = sde_opt
        .and_then(|sde| sde.dungeons.get(&did))
        .and_then(|d| d.faction_id)
        .filter(|&f| f > 0);
    let eff_corp = if corp_id > 0 {
        Some(i64::from(corp_id))
    } else {
        let fid_opt = if faction_id > 0 { Some(faction_id) } else { dungeon_faction };
        fid_opt.and_then(|fid| {
            sde_opt
                .and_then(|sde| sde.factions.get(&fid))
                .and_then(|f| f.corporation_id.or(f.militia_corporation_id))
                .filter(|&c| c > 0)
        })
    };
    let eff_standing_from = if faction_id > 0 {
        Some(i64::from(faction_id))
    } else if corp_id > 0 {
        Some(i64::from(corp_id))
    } else {
        dungeon_faction.map(i64::from)
    };
    let (mut wallet_opt, mut lp_ok, mut standing_opt) = (None, false, None);
    if let Some(ch) = snap.characters.get_mut(&recip) {
        if isk > 0 {
            if let Some(cents) = isk.checked_mul(100).map(IskCents::from_raw_cents) {
                if let Some(next_bal) = ch.balance.checked_add(cents) { ch.balance = next_bal; wallet_opt = Some((cents, next_bal)); }
            }
        }
        if let (true, Some(cid)) = (lp > 0, eff_corp) {
            let ent = ch.loyalty_points.entry(cid).or_insert(0);
            *ent = ent.saturating_add(lp);
            lp_ok = true;
        }
        if let (true, Some(from_id)) = (standing_milli != 0, eff_standing_from) {
            let pct = (f64::from(standing_milli.unsigned_abs()) / 100.0).clamp(0.0, 100.0);
            let cur = *ch.standings.get(&from_id).unwrap_or(&0.0);
            let next = if standing_milli > 0 { crate::formulas::apply_standing_increase(cur, pct) } else { crate::formulas::apply_standing_decrease(cur, pct) };
            ch.standings.insert(from_id, next);
            standing_opt = Some((from_id, next - cur));
        }
    }
    if let Some((amount, balance_after)) = wallet_opt {
        let jid = snap.alloc_entity_id();
        snap.wallet_journal.push(WalletJournalEntry { entry_id: jid, character_id: recip, entry_type: "dungeon_reward".into(), amount, balance_after, reason: format!("Dungeon {did} reward") });
        snap.wallet_journal_timestamps.insert(jid, now_unix);
    }
    if let (true, Some(cid)) = (lp_ok && lp > 0, eff_corp) {
        crate::services::lp_store::record_lp_reward_log(snap, recip, cid, lp, format!("Dungeon {did} reward"), now_unix);
    }
    if let Some((from_id, delta)) = standing_opt {
        let eid = snap.alloc_entity_id();
        snap.standing_transactions.entry(recip).or_default().push(StandingTransactionRecord {
            event_id: eid, from_id, to_id: recip, modification: delta, event_type_id: if standing_milli > 0 { 150 } else { 151 },
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn grant_loot_in_snap(
    snap: &mut WorldStateSnapshot, sys_id: SolarSystemId, inst: &mut DungeonRuntimeInstance,
    r_idx: usize, items: &[(u32, i32)], target_obj: Option<usize>, recipient: i64, effects: &mut TriggerTxEffects,
) {
    let valid: Vec<(u32, i32)> = items.iter().copied().filter(|&(t, q)| t > 0 && q > 0).collect();
    if valid.is_empty() { return; }
    let owner = if recipient > 0 { recipient } else { inst.owner_char_id };
    let Some(room) = inst.rooms.get_mut(r_idx) else { return };
    // R5 P2-4: reject loot injection into unspawned or destroyed containers.
    let target = if let Some(oi) = target_obj {
        room.objects.get_mut(oi).filter(|o| o.is_container && o.spawned && !o.destroyed && o.ball_id > 0)
            .map(|o| { o.loot_items.extend(valid.iter().copied()); (o.ball_id, o.type_id, o.position) })
    } else if let Some(o) = room.objects.iter_mut().find(|o| o.is_container && o.spawned && !o.destroyed && o.ball_id > 0) {
        o.loot_items.extend(valid.iter().copied());
        Some((o.ball_id, o.type_id, o.position))
    } else {
        let cid = snap.alloc_entity_id();
        let pos = [room.position[0] + 500.0, room.position[1], room.position[2]];
        room.objects.push(DungeonRuntimeObject {
            ball_id: cid, type_id: DEFAULT_CONTAINER_TYPE_ID, name: "Dungeon Reward Cache".into(),
            position: pos, radius: 150.0, is_container: true, loot_items: valid.clone(), spawned: true, ..Default::default()
        });
        effects.spawn_objects.push(ObjectSpawnOrder { ball_id: cid, type_id: DEFAULT_CONTAINER_TYPE_ID, position: pos, radius: 150.0, destructible: false });
        Some((cid, DEFAULT_CONTAINER_TYPE_ID, pos))
    };
    if let Some((bid, tid, pos)) = target { insert_container_and_loot_in_snap(snap, sys_id, bid, tid, pos, &valid, owner); }
}

fn apply_set_objective_in_inst(
    inst: &mut DungeonRuntimeInstance, r_idx: usize, obj_id: i64, key: &str, title: &str,
    completed: bool, recip: i64, effects: &mut TriggerTxEffects,
) -> i64 {
    let room_idx_val = inst.rooms.get(r_idx).map(|r| r.room_index);
    let mut resolved = obj_id;
    if let Some(ex) = inst.objectives.iter_mut().find(|o| (obj_id > 0 && o.objective_id == obj_id) || (!key.is_empty() && o.key == key)) {
        if !title.is_empty() { ex.title = title.to_string(); }
        ex.status = if completed { DungeonObjectiveStatus::Completed } else { DungeonObjectiveStatus::Active };
        resolved = ex.objective_id;
    } else {
        if resolved <= 0 { resolved = i64::from(inst.dungeon_id) * 100 + (inst.objectives.len() as i64) + 1; }
        inst.objectives.push(DungeonObjective {
            objective_id: resolved, key: key.to_string(), title: title.to_string(), room_index: room_idx_val,
            status: if completed { DungeonObjectiveStatus::Completed } else { DungeonObjectiveStatus::Active }, ..Default::default()
        });
    }
    inst.active_objective_id = resolved;
    if completed {
        if !inst.completed_task_ids.contains(&resolved) {
            inst.completed_task_ids.push(resolved);
        }
        inst.last_completed_task_id = Some(resolved);
    }
    if recip > 0 {
        if completed {
            effects.progression_notices.push(ProgressionNotice::TaskEvents(recip, vec![(resolved, resolved)]));
        } else {
            effects.progression_notices.push(ProgressionNotice::AdvancedOnly(recip, resolved, inst.last_completed_task_id.unwrap_or(0)));
        }
    }
    resolved
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_single_action_in_inst(
    snap: &mut WorldStateSnapshot, sys_id: SolarSystemId, inst: &mut DungeonRuntimeInstance,
    sde_opt: Option<&eve_sde::SdeRegistry>, src_r_idx: usize, target_room_index: Option<usize>,
    trig_id: u32, trig_rep: bool, act: &DungeonTriggerAction, actor: i64, now_unix: u64, effects: &mut TriggerTxEffects,
) -> Option<(i64, bool)> {
    let r_idx = match target_room_index {
        Some(ri) if ri < inst.rooms.len() => ri,
        Some(_) => return None,
        None => src_r_idx,
    };
    let room_id = inst.rooms.get(r_idx).map(|r| r.room_id).unwrap_or(0);
    let recip = resolve_room_recipient(inst, Some(r_idx), actor);
    let default_oid = room_id * 10 + i64::from(trig_id);
    let mut explicit_obj = false;
    let oid = match act {
        DungeonTriggerAction::UnlockGate | DungeonTriggerAction::LockGate => {
            let locked = matches!(act, DungeonTriggerAction::LockGate);
            if locked { effects.locked_rooms.insert(r_idx); } else { effects.locked_rooms.remove(&r_idx); }
            if let Some(r) = inst.rooms.get_mut(r_idx) {
                r.gate_locked = locked;
                r.gate_explicitly_locked = locked;
                for gl in &mut r.gate_links { gl.locked = locked; }
            }
            if locked { default_oid } else { room_id * 10 + 9 }
        }
        DungeonTriggerAction::UnlockEntryGate => { inst.entry_gate_locked = false; default_oid }
        DungeonTriggerAction::SpawnNpcGroup(gid) => {
            if let Some(r) = inst.rooms.get_mut(r_idx) {
                if let Some(gi) = r.npc_groups.iter().position(|g| g.group_id == *gid && (!g.spawned || ((g.repeatable || trig_rep) && g.cleared && g.npc_ball_ids.is_empty()))) {
                    plan_npc_group_spawns_in_snap(snap, r, gi, now_unix, &mut effects.spawn_npcs);
                }
            }
            room_id * 10 + i64::from(*gid)
        }
        DungeonTriggerAction::DespawnNpcGroup(gid) => {
            if let Some(r) = inst.rooms.get_mut(r_idx) {
                let mut any_despawned = false;
                for g in &mut r.npc_groups {
                    if g.group_id == *gid {
                        any_despawned = true;
                        for bid in g.npc_ball_ids.drain(..) {
                            if !g.dead_npc_ball_ids.contains(&bid) { g.dead_npc_ball_ids.push(bid); }
                            effects.despawn_npc_ids.push(bid);
                        }
                        g.npc_ball_meta.clear(); g.npc_ball_tags.clear(); g.npc_ball_names.clear(); g.npc_ball_bounties.clear();
                        g.spawned = true; g.cleared = true;
                    }
                }
                if any_despawned {
                    r.armed_timers.entry(format!("__group_cleared:{gid}")).or_insert(now_unix);
                }
            }
            if r_idx != src_r_idx && !effects.affected_clear_rooms.contains(&r_idx) {
                effects.affected_clear_rooms.push(r_idx);
            }
            room_id * 10 + i64::from(*gid)
        }
        DungeonTriggerAction::SpawnObject { object_index } => {
            let cont = inst.rooms.get_mut(r_idx).and_then(|r| r.objects.get_mut(*object_index)).and_then(|o| {
                if o.destroyed || o.spawned { return None; }
                if o.ball_id <= 0 { o.ball_id = snap.alloc_entity_id(); }
                o.spawned = true;
                effects.spawn_objects.push(ObjectSpawnOrder { ball_id: o.ball_id, type_id: o.type_id, position: o.position, radius: o.radius, destructible: o.destructible });
                (o.is_container && !snap.items.contains_key(&o.ball_id)).then(|| (o.ball_id, o.type_id, o.position, o.loot_items.clone()))
            });
            if let Some((bid, tid, pos, items)) = cont {
                insert_container_and_loot_in_snap(snap, sys_id, bid, tid, pos, &items, if recip > 0 { recip } else { inst.owner_char_id });
            }
            default_oid
        }
        DungeonTriggerAction::GrantLoot { items, target_object_index } => {
            grant_loot_in_snap(snap, sys_id, inst, r_idx, items, *target_object_index, recip, effects); default_oid
        }
        DungeonTriggerAction::GrantReward { isk, lp, corp_id, faction_id, standing_milli } => {
            grant_reward_in_snap(snap, sde_opt, inst.dungeon_id, recip, *isk, *lp, *corp_id, *faction_id, *standing_milli, now_unix); default_oid
        }
        DungeonTriggerAction::CompleteDungeon => {
            if let Some(r) = inst.rooms.get_mut(r_idx) { r.cleared = true; }
            effects.complete_now = true; room_id * 10 + 99
        }
        DungeonTriggerAction::FailDungeon { reason } => {
            effects.fail_reason = Some(reason.clone());
            default_oid
        }
        DungeonTriggerAction::StartTimer { timer_id, delay_secs } => {
            if !timer_id.is_empty() && !timer_id.starts_with("__") {
                if let Some(r) = inst.rooms.get_mut(r_idx) {
                    r.armed_timers.insert(timer_id.clone(), now_unix.saturating_add(*delay_secs));
                }
            }
            default_oid
        }
        DungeonTriggerAction::SetObjective { objective_id, objective_key, title, completed } => {
            explicit_obj = true;
            apply_set_objective_in_inst(inst, r_idx, *objective_id, objective_key, title, *completed, recip, effects)
        }
        DungeonTriggerAction::StartEscalation { target_dungeon_id, chance_bps, target_title } => {
            if recip > 0 {
                let pos = inst.rooms.get(r_idx).map(|r| r.position).or(inst.entry_gate_pos).unwrap_or(inst.base_position);
                effects.escalation_requests.push((recip, inst.dungeon_id, *target_dungeon_id, *chance_bps, target_title.clone(), pos, inst.instance_id));
            }
            default_oid
        }
        n @ (DungeonTriggerAction::CameraInterest { .. } | DungeonTriggerAction::TriggerAmbientSound { .. }
        | DungeonTriggerAction::TriggerAudio { .. } | DungeonTriggerAction::TriggerMessage { .. } | DungeonTriggerAction::TriggerMusic { .. }) => {
            if recip > 0 {
                let oids = inst.rooms.get(r_idx).map(|r| r.objects.iter().map(|o| o.ball_id).collect()).unwrap_or_default();
                effects.notify_actions.push((recip, inst.dungeon_id, oids, vec![n.clone()]));
            }
            default_oid
        }
    };
    Some((oid, explicit_obj))
}

/// Evaluates room clear, gate unlocks, and dungeon auto-complete STRICTLY AFTER triggers execute (`OWN-R5-02`).
pub(crate) fn unlock_room_gates_on_clear_in_inst(
    inst: &mut DungeonRuntimeInstance, r_idx: usize, actor: i64,
    had_room_actions: bool, allow_empty_groups_clear: bool, effects: &mut TriggerTxEffects,
) {
    if effects.complete_now || effects.fail_reason.is_some() || instance_is_finished(inst) { return; }
    let total_rooms = inst.rooms.len();
    let Some(room) = inst.rooms.get_mut(r_idx) else { return };
    if !allow_empty_groups_clear && room.npc_groups.is_empty() { return; }
    let all_dead = room.npc_groups.iter().filter(|g| g.spawned).all(|g| g.npc_ball_ids.is_empty());
    let all_spawned = room.npc_groups.iter().all(|g| g.spawned);
    let any_blocking = room.triggers.iter().any(|t| (!t.fired && !t.repeatable) || t.armed_at_unix.is_some());
    if !(all_dead && all_spawned && !any_blocking) { return; }

    let was_cleared = room.cleared;
    room.cleared = true;
    if !was_cleared && !room.gate_explicitly_locked && !effects.locked_rooms.contains(&r_idx) {
        room.gate_locked = false;
        for gl in &mut room.gate_links { gl.locked = false; }
    }
    if !was_cleared && !had_room_actions {
        let oid = if r_idx + 1 < total_rooms { room.room_id * 10 + 9 } else { room.room_id * 10 + 99 };
        let evs = vec![(room.room_id * 100 + 99, oid)];
        crate::services::pve_dungeon_runtime_rpc::record_dungeon_progression_events(inst, &evs);
        let recip = resolve_room_recipient(inst, Some(r_idx), actor);
        if recip > 0 {
            effects.progression_notices.push(ProgressionNotice::TaskEvents(recip, evs));
        }
    }
    if r_idx + 1 == total_rooms
        && !inst.rooms[r_idx].gate_locked
        && !effects.locked_rooms.contains(&r_idx)
        && inst.rooms.iter().all(|r| r.cleared && r.npc_groups.iter().all(|g| g.spawned && g.cleared && g.npc_ball_ids.is_empty()))
    {
        effects.complete_now = true;
    }
}

pub(crate) fn finish_room_clear_checks_in_inst(
    inst: &mut DungeonRuntimeInstance, primary_r_idx: usize, actor: i64,
    had_room_actions: bool, allow_empty_groups_clear: bool, effects: &mut TriggerTxEffects,
) {
    unlock_room_gates_on_clear_in_inst(inst, primary_r_idx, actor, had_room_actions, allow_empty_groups_clear, effects);
    let cross_rooms = std::mem::take(&mut effects.affected_clear_rooms);
    for cr_idx in cross_rooms {
        if cr_idx != primary_r_idx {
            unlock_room_gates_on_clear_in_inst(inst, cr_idx, actor, true, false, effects);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_trigger_batch_in_inst(
    snap: &mut WorldStateSnapshot, sys_id: SolarSystemId, inst: &mut DungeonRuntimeInstance,
    sde_opt: Option<&eve_sde::SdeRegistry>, r_idx: usize, fallback_actor: i64, now_unix: u64,
    batch: TriggerBatch, effects: &mut TriggerTxEffects,
) -> (usize, usize) {
    let source_room_id = inst.rooms.get(r_idx).map(|r| r.room_id).unwrap_or(0);
    let (mut executed_triggers, mut fired_actions) = (0usize, 0usize);
    for (trig_id, target_room_index, trig_rep, trig_actor, actions) in batch {
        if effects.fail_reason.is_some() || effects.complete_now { break; }
        if actions.is_empty() { continue; }
        executed_triggers += 1;
        let eff_actor = if trig_actor > 0 { trig_actor } else { fallback_actor };
        let recip = resolve_room_recipient(inst, Some(r_idx), eff_actor);
        let (mut primary_oid, mut had_explicit_objective) = (None, false);
        for (idx, act) in actions.into_iter().enumerate() {
            let Some((oid, is_exp)) = apply_single_action_in_inst(
                snap, sys_id, inst, sde_opt, r_idx, target_room_index, trig_id, trig_rep, &act, eff_actor, now_unix, effects,
            ) else { continue; };
            fired_actions += 1;
            if is_exp {
                had_explicit_objective = true;
            } else if primary_oid.is_none() && idx == 0 {
                primary_oid = Some(oid);
            }
        }
        if !had_explicit_objective && effects.fail_reason.is_none() {
            if let Some(oid) = primary_oid {
                let evs = vec![(source_room_id * 100 + i64::from(trig_id), oid)];
                crate::services::pve_dungeon_runtime_rpc::record_dungeon_progression_events(inst, &evs);
                if recip > 0 {
                    effects.progression_notices.push(ProgressionNotice::TaskEvents(recip, evs));
                }
            }
        }
    }
    (executed_triggers, fired_actions)
}

pub(crate) fn finalize_terminal_in_snap(
    snap: &mut WorldStateSnapshot, inst: DungeonRuntimeInstance, actor: i64, now_unix: u64, effects: &mut TriggerTxEffects,
) {
    let inst_id = inst.instance_id;
    snap.dungeon_runtime.instances.insert(inst_id, inst);
    if effects.fail_reason.is_some() {
        finish_instance_entities_in_snap(snap, inst_id, actor, now_unix, true, true, effects);
    } else if effects.complete_now {
        finish_instance_entities_in_snap(snap, inst_id, actor, now_unix, false, false, effects);
    }
    crate::services::agents_missions::agents_missions_dungeon::sync_mission_from_dungeon_in_snap(snap, inst_id);
    effects.cancel_dead_spawns();
}

#[cfg(test)]
pub(crate) async fn apply_dungeon_trigger_actions_impl(
    deps: DungeonRuntimeDeps<'_>, sys_id: SolarSystemId, instance_id: i64, room_vec_idx: usize,
    actions: Vec<(u32, DungeonTriggerAction)>, char_id: i64,
) -> usize {
    if actions.is_empty() { return 0; }
    let now_unix = deps.db.clock().now_unix_secs();
    let (mut effects, mut fired) = (TriggerTxEffects::default(), 0usize);
    deps.db.with_mut_snapshot(|snap| {
        let Some(mut inst) = snap.dungeon_runtime.instances.remove(&instance_id) else { return };
        if instance_is_finished(&inst) { snap.dungeon_runtime.instances.insert(instance_id, inst); return; }
        super::record_room_actor(&mut inst, room_vec_idx, char_id);
        let batch: TriggerBatch = actions.into_iter().map(|(tid, a)| (tid, None, false, char_id, vec![a])).collect();
        fired = run_trigger_batch_in_inst(snap, sys_id, &mut inst, deps.sde, room_vec_idx, char_id, now_unix, batch, &mut effects).1;
        finalize_terminal_in_snap(snap, inst, char_id, now_unix, &mut effects);
    });
    flush_trigger_effects(deps, sys_id, effects).await;
    fired
}

pub(crate) async fn complete_and_flush_instance(
    deps: DungeonRuntimeDeps<'_>, sys_id: SolarSystemId, instance_id: i64, char_id: i64, force_full_despawn: bool,
) {
    let now_unix = deps.db.clock().now_unix_secs();
    let mut effects = TriggerTxEffects::default();
    deps.db.with_mut_snapshot(|snap| {
        finish_instance_entities_in_snap(snap, instance_id, char_id, now_unix, false, force_full_despawn, &mut effects);
        effects.cancel_dead_spawns();
    });
    flush_trigger_effects(deps, sys_id, effects).await;
}

pub(crate) fn merge_effects(dst: &mut TriggerTxEffects, mut src: TriggerTxEffects) {
    src.cancel_dead_spawns();
    dst.despawn_npc_ids.extend(src.despawn_npc_ids); dst.despawn_other_balls.extend(src.despawn_other_balls);
    dst.spawn_gates.extend(src.spawn_gates); dst.spawn_objects.extend(src.spawn_objects); dst.spawn_npcs.extend(src.spawn_npcs);
    dst.notify_actions.extend(src.notify_actions); dst.progression_notices.extend(src.progression_notices);
    dst.completed_sites.extend(src.completed_sites);
    dst.failed_notices.extend(src.failed_notices); dst.escalation_requests.extend(src.escalation_requests);
    dst.cancel_dead_spawns();
}

fn flush_trigger_notifications_and_completion_sync(
    deps: DungeonRuntimeDeps<'_>,
    sys_id: SolarSystemId,
    notify_actions: Vec<(i64, u32, Vec<i64>, Vec<DungeonTriggerAction>)>,
    progression_notices: Vec<ProgressionNotice>,
    failed_notices: Vec<(i64, u32, i64)>,
    completed_sites: Vec<(i64, u32, i64, bool)>,
) {
    for (recip, did, obj_ids, acts) in notify_actions {
        if recip > 0 && !acts.is_empty() {
            crate::services::notify_emitters_channel_events::dispatch_dungeon_trigger_notify_actions(deps.db, deps.pending_notifications, deps.notify_tx, recip, i64::from(did), &obj_ids, acts);
        }
    }
    for notice in progression_notices {
        match notice {
            ProgressionNotice::TaskEvents(recip, evs) => {
                if recip > 0 && !evs.is_empty() {
                    crate::services::pve_dungeon_runtime_rpc::dispatch_dungeon_notify_actions(deps.db, deps.pending_notifications, deps.notify_tx, recip, &evs);
                }
            }
            ProgressionNotice::AdvancedOnly(recip, oid, tid) => {
                if recip > 0 {
                    crate::services::notify_emitters_ess_bounties::emit_progression_advanced_with_db(deps.db, deps.pending_notifications, deps.notify_tx, recip, oid, tid);
                }
            }
        }
    }
    for (cid, did, iid) in failed_notices {
        if cid > 0 {
            crate::router::notify_registry::emit_typed_char_with_db(deps.db, deps.pending_notifications, deps.notify_tx, cid, crate::router::notify_registry::OnDungeonExited { dungeon_id: i64::from(did), instance_id: iid });
        }
    }
    let now_unix = deps.db.clock().now_unix_secs();
    for (_, did, recip, clear_exploration) in completed_sites {
        super::ops::finalize_completed_dungeon_effects_sync(deps.db, deps.pending_notifications, deps.notify_tx, sys_id, did, recip, true, clear_exploration, now_unix);
    }
}

pub(crate) fn flush_trigger_effects_sync(deps: DungeonRuntimeDeps<'_>, sys_id: SolarSystemId, mut effects: TriggerTxEffects) {
    effects.cancel_dead_spawns();
    let sys_handle_opt = deps.systems.get_handle(sys_id);
    despawn_dungeon_npc_ids_sync(deps, sys_handle_opt.as_ref(), &effects.despawn_npc_ids);
    if let Some(sh) = sys_handle_opt.as_ref() { for bid in &effects.despawn_other_balls { let _ = sh.remove_ball_sync(*bid); } }
    if !effects.spawn_gates.is_empty() || !effects.spawn_objects.is_empty() || !effects.spawn_npcs.is_empty() {
        let sh = wake_dungeon_system_handle(deps.systems, sys_id);
        for (gid, gpos, tid) in effects.spawn_gates { spawn_gate_ball_sync(&sh, gid, gpos, tid); }
        if let Some(sde) = deps.sde {
            for obj in effects.spawn_objects { spawn_object_ball_sync(&sh, sde, obj.ball_id, obj.type_id, obj.position, obj.radius, obj.destructible); }
        }
        if !effects.spawn_npcs.is_empty() { spawn_dungeon_npc_orders_sync(deps, sys_id, &sh, effects.spawn_npcs); }
    }
    flush_trigger_notifications_and_completion_sync(
        deps, sys_id, effects.notify_actions, effects.progression_notices, effects.failed_notices, effects.completed_sites,
    );
}

pub(crate) async fn flush_trigger_effects(deps: DungeonRuntimeDeps<'_>, sys_id: SolarSystemId, mut effects: TriggerTxEffects) {
    effects.cancel_dead_spawns();
    let sys_handle_opt = deps.systems.get_handle(sys_id);
    despawn_dungeon_npc_ids(deps, sys_handle_opt.as_ref(), &effects.despawn_npc_ids).await;
    if let Some(sh) = sys_handle_opt.as_ref() { for bid in &effects.despawn_other_balls { let _ = sh.remove_ball(*bid).await; } }
    if !effects.spawn_gates.is_empty() || !effects.spawn_objects.is_empty() || !effects.spawn_npcs.is_empty() {
        let sh = wake_dungeon_system_handle(deps.systems, sys_id);
        for (gid, gpos, tid) in effects.spawn_gates { spawn_gate_ball(&sh, gid, gpos, tid).await; }
        if let Some(sde) = deps.sde {
            for obj in effects.spawn_objects { spawn_object_ball(&sh, sde, obj.ball_id, obj.type_id, obj.position, obj.radius, obj.destructible).await; }
        }
        if !effects.spawn_npcs.is_empty() { spawn_dungeon_npc_orders(deps, sys_id, &sh, effects.spawn_npcs).await; }
    }
    flush_trigger_notifications_and_completion_sync(
        deps, sys_id, effects.notify_actions, effects.progression_notices, effects.failed_notices, effects.completed_sites,
    );
    for (cid, src_did, target_did, chance_bps, target_title, pos, inst_id) in effects.escalation_requests {
        let _ = dispatch_dungeon_escalation_request(
            deps, sys_id, cid, src_did, target_did, chance_bps, target_title, pos, inst_id,
        )
        .await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_dungeon_escalation_request(
    deps: DungeonRuntimeDeps<'_>,
    sys_id: SolarSystemId,
    cid: i64,
    src_did: u32,
    target_did: Option<u32>,
    chance_bps: u16,
    target_title: Option<String>,
    pos: [f64; 3],
    inst_id: i64,
) -> bool {
    let Some(sde_arc) = deps.sde_arc else {
        return false;
    };
    if cid <= 0
        || chance_bps == 0
        || (chance_bps < 10_000
            && deterministic_roll_bps(inst_id ^ cid, (u64::from(src_did) << 16) ^ u64::from(chance_bps))
                >= chance_bps)
    {
        return false;
    }
    if let Some(entry) = create_escalation_expedition_with_options_core(
        deps.db,
        sde_arc,
        deps.systems,
        cid,
        sys_id,
        src_did,
        target_did,
        target_title.as_deref(),
        pos,
    )
    .await
    {
        finalize_runtime_escalation(deps, sde_arc, inst_id, cid, entry).await;
        true
    } else {
        false
    }
}

pub(crate) async fn flush_pending_dungeon_escalations_impl(deps: DungeonRuntimeDeps<'_>) -> usize {
    if deps
        .db
        .with_snapshot(|snap| snap.dungeon_runtime.pending_escalations.is_empty())
    {
        return 0;
    }
    let pending = deps
        .db
        .with_mut_snapshot(|snap| std::mem::take(&mut snap.dungeon_runtime.pending_escalations));
    let mut created = 0usize;
    for req in pending {
        if dispatch_dungeon_escalation_request(
            deps,
            req.solar_system_id,
            req.char_id,
            req.source_dungeon_id,
            req.target_dungeon_id,
            req.chance_bps,
            req.target_title,
            req.position,
            req.instance_id,
        )
        .await
        {
            created += 1;
        }
    }
    created
}

async fn finalize_runtime_escalation(
    deps: DungeonRuntimeDeps<'_>, sde_arc: &Arc<eve_sde::SdeRegistry>,
    source_inst_id: i64, char_id: i64, entry: crate::services::pve_expeditions::EscalatingPathEntry,
) {
    let exp_pos = [-45_000_000.0 + (entry.step_index as f64) * 5_000_000.0, 15_000_000.0, 30_000_000.0];
    let sig_id = expedition_sig_id(entry.instance_id, entry.step_index);
    let scan_strength = sde_arc.dungeons.get(&entry.expedition_dungeon_id).and_then(|d| d.difficulty)
        .map(|diff| (1.0 / (diff.max(1) as f64)).clamp(0.05, 1.0)).unwrap_or(0.5);
    let fleet_info = deps.db.with_mut_snapshot(|snap| {
        if let Some(inst) = snap.dungeon_runtime.instances.get_mut(&source_inst_id) { inst.escalation_instance_id = Some(entry.instance_id); }
        let list = snap.cosmic_signatures.entry(entry.destination_solar_system_id).or_default();
        if !list.iter().any(|s| s.signature_id == sig_id) {
            list.push(eve_db::types::CosmicSignatureRecord {
                signature_id: sig_id.clone(), solar_system_id: entry.destination_solar_system_id, sig_group: "Combat".into(),
                type_name: entry.expedition_name.clone(), position: exp_pos, signal_strength: scan_strength,
                created_unix: entry.created_at_sec, lifetime_seconds: 86_400, ..Default::default()
            });
        }
        let fid = snap.characters.get(&char_id).and_then(|c| c.fleet_id).filter(|&f| f > 0)?;
        let members = snap.fleets.get(&fid).map(|fl| fl.members.clone()).unwrap_or_default();
        Some((fid, members, i64::from(entry.effective_journal_entry_template_id())))
    });
    if let Some(ctx) = deps.ctx {
        let _ = Box::pin(crate::services::pve_dungeon_runtime_rpc::spawn_sde_dungeon_runtime_instance(
            ctx, char_id, entry.destination_solar_system_id, entry.expedition_dungeon_id, "Expedition", exp_pos, false,
        )).await;
    }
    crate::router::notify_registry::emit_typed_char_with_db(deps.db, deps.pending_notifications, deps.notify_tx, char_id, crate::router::notify_registry::OnEscalatingPathChange);
    crate::router::notify_registry::emit_typed_char_with_db(deps.db, deps.pending_notifications, deps.notify_tx, char_id, crate::router::notify_registry::OnEscalatingPathMessage { instance_id: entry.instance_id });
    if let Some((fid, members, template_id)) = fleet_info {
        crate::router::notify_registry::emit_typed_to_fleet_members_core(deps.db, deps.pending_notifications, deps.notify_tx, fid, &members, crate::router::notify_registry::OnFleetEscalatingPathMessage { char_id, journal_entry_template_id: template_id });
    }
    if let (Some(pend), Some(tx)) = (deps.pending_notifications, deps.notify_tx) {
        crate::router::notify_registry::emit_typed_to_solar_system_in_space_core(deps.db, pend, tx, entry.destination_solar_system_id, crate::router::notify_registry::OnCosmicSignatureAdded { target_id: sig_id.clone() });
        let instance = PyRep::keyval([("instanceID", PyRep::Integer(entry.instance_id)), ("dungeonID", PyRep::Integer(i64::from(entry.expedition_dungeon_id))), ("signatureID", PyRep::String(sig_id)), ("scanStrengthValue", PyRep::Real(scan_strength))]);
        crate::router::notify_registry::emit_typed_to_solar_system_in_space_core(deps.db, pend, tx, entry.destination_solar_system_id, crate::router::notify_registry::OnDungeonInstanceAdded { solar_system_id: i64::from(entry.destination_solar_system_id), instance });
    }
}
