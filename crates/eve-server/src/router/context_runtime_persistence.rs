//! Durable per-system persistence and rollback rehydration for `ServerContext` runtime state (`#592`, `#920`).
//!
//! Keeps transient network/handle/derived caches (`session_closers`, `notify_tx`,
//! `pending_notifications`, `bound_objects`, `object_cache*`, `ship_fittings`) and explicitly
//! crash-lost runtime kinds (`module_cycles`, `target_locks`, `active_warps`, `npcs`) out of
//! periodic checkpoints while persisting declared checkpointed runtime state per solar system in
//! SQLite `system_checkpoint(system_id, kind, key, value)`.

use std::collections::{HashMap, HashSet};

use super::context::{
    ActiveFighterSquadron, ActiveMoonSurveyProbe, ActiveScannerProbe, ModuleCooldownEntry,
    ServerContext,
};
use crate::services::combat_crimewatch::{
    flush_combat_runtime_to_db, rehydrate_combat_runtime_from_db_forced,
};
use crate::services::fleet_social::fleet_social_fleets::{FleetAdvertState, FleetSetupRecord};
use crate::services::fleet_social::fleet_social_persistence::{
    hydrate_social_state_from_db, persist_social_state,
};
use crate::services::space_mining_harvest::PhasedAsteroidFieldState;
use crate::services::tactical_module_mechanics::{ActiveInterdictionProbe, LauncherFireStamp};
use crate::services::tactical_modules::{
    db_ptr_key, ProjectedRemoteAssistance, TacticalModulesState,
};
use crate::services::tactical_modules_probes::{ActiveWubbleDebuff, PendingWubbleProbe};
use eve_db::sqlite_store::domain_rows::{from_checked_domain_json, to_checked_domain_json};
use eve_db::sqlite_store::system_checkpoint::{
    current_unix_secs, ensure_system_checkpoint_schema, has_any_system_checkpoints,
    load_all_checkpoints_for_kind, purge_crash_lost_checkpoints, replace_checkpoint_kinds_batch,
    retire_legacy_server_runtime_blobs, CLUSTER_SYSTEM_ID, KIND_FLEET_EXTRAS, KIND_MAP_LANDMARKS,
    KIND_MOON_PROBES, KIND_PHASED_ASTEROID_FIELDS, KIND_SCAN_PROGRESS,
    KIND_SCAN_SIGNATURES, KIND_SOCIAL_EXTRAS, KIND_SYSTEM_SIM_TIME, KIND_TACTICAL_DURABLE,
};
use eve_db::{FleetRole, SessionChangeTimerRecord, WorldStateSnapshot};

pub(crate) const SERVER_RUNTIME_STATE_BLOB_KEY: &str = "server_runtime_state";

#[cfg(test)]
fn intern_ewar_kind(kind: &str) -> &'static str {
    match kind {
        "warpScramblerMWD" => "warpScramblerMWD",
        "warpScrambler" => "warpScrambler",
        "focusedWarpScrambler" => "focusedWarpScrambler",
        "webify" => "webify",
        "electronic" => "electronic",
        "ewRemoteSensorDamp" => "ewRemoteSensorDamp",
        "ewGuidanceDisrupt" => "ewGuidanceDisrupt",
        "ewTrackingDisrupt" => "ewTrackingDisrupt",
        "ewTargetPaint" => "ewTargetPaint",
        "ewEnergyNeut" => "ewEnergyNeut",
        "ewEnergyVampire" => "ewEnergyVampire",
        _ => "electronic",
    }
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct PersistedTacticalModulesPayload {
    /// Tag: `0` => keyed by `db_ptr_key`, `1` => keyed by `instance_id`.
    remote_assistance: Vec<((u8, i64), Vec<ProjectedRemoteAssistance>)>,
    lance_caster_lock_until: Vec<((u8, i64), u64)>,
    lance_victim_debuff_until: Vec<((u8, i64), u64)>,
    pending_wubble_probes: Vec<(u8, PendingWubbleProbe)>,
    active_wubble_debuffs: Vec<((u8, i64), ActiveWubbleDebuff)>,
    tsb_self_penalty_until: Vec<((u8, i64), u64)>,
    active_interdiction_probes: Vec<((u8, i64), ActiveInterdictionProbe)>,
    launcher_last_fire: Vec<((u8, i64), LauncherFireStamp)>,
    system_sim_time_s: Vec<((u8, u32), f64)>,
}

impl PersistedTacticalModulesPayload {
    #[cfg(test)]
    fn from_state(st: &TacticalModulesState, cur_inst_key: usize) -> Self {
        Self::from_state_for_keys(st, None, cur_inst_key)
    }

    #[cfg(test)]
    fn from_state_for_keys(
        st: &TacticalModulesState,
        cur_db_key: Option<usize>,
        cur_inst_key: usize,
    ) -> Self {
        let keep_key = |k: usize| -> bool {
            cur_db_key.is_none_or(|db_key| k == db_key || k == cur_inst_key)
        };
        let tag_of = |k: usize| -> u8 { u8::from(k == cur_inst_key) };
        let mut payload = Self {
            remote_assistance: st
                .remote_assistance
                .iter()
                .filter(|(&(k, _), _)| keep_key(k))
                .map(|(&(k, sid), v)| ((tag_of(k), sid), v.clone()))
                .collect(),
            lance_caster_lock_until: Vec::new(),
            lance_victim_debuff_until: Vec::new(),
            pending_wubble_probes: st
                .pending_wubble_probes
                .iter()
                .filter(|(k, _)| keep_key(*k))
                .map(|(k, v)| (tag_of(*k), v.clone()))
                .collect(),
            active_wubble_debuffs: st
                .active_wubble_debuffs
                .iter()
                .filter(|(&(k, _), _)| keep_key(k))
                .map(|(&(k, sid), v)| ((tag_of(k), sid), v.clone()))
                .collect(),
            tsb_self_penalty_until: Vec::new(),
            active_interdiction_probes: st
                .active_interdiction_probes
                .iter()
                .filter(|(&(k, _), _)| keep_key(k))
                .map(|(&(k, pid), v)| ((tag_of(k), pid), v.clone()))
                .collect(),
            launcher_last_fire: st
                .launcher_last_fire
                .iter()
                .filter(|(&(k, _), _)| keep_key(k))
                .map(|(&(k, mid), &v)| ((tag_of(k), mid), v))
                .collect(),
            system_sim_time_s: st
                .system_sim_time_s
                .iter()
                .filter(|(&(k, _), _)| keep_key(k))
                .map(|(&(k, sys), &v)| ((tag_of(k), sys), v))
                .collect(),
        };
        payload.remote_assistance.sort_by_key(|(k, _)| *k);
        payload.active_wubble_debuffs.sort_by_key(|(k, _)| *k);
        payload.active_interdiction_probes.sort_by_key(|(k, _)| *k);
        payload.launcher_last_fire.sort_by_key(|(k, _)| *k);
        payload.system_sim_time_s.sort_by_key(|(k, _)| *k);
        payload
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.remote_assistance.is_empty()
            && self.lance_caster_lock_until.is_empty()
            && self.lance_victim_debuff_until.is_empty()
            && self.pending_wubble_probes.is_empty()
            && self.active_wubble_debuffs.is_empty()
            && self.tsb_self_penalty_until.is_empty()
            && self.active_interdiction_probes.is_empty()
            && self.launcher_last_fire.is_empty()
            && self.system_sim_time_s.is_empty()
    }

    fn apply_to_state(
        self,
        st: &mut TacticalModulesState,
        cur_db_key: usize,
        cur_inst_key: usize,
    ) -> LegacyTacticalDurableEntries {
        let untag = |tag: u8| -> usize { if tag == 1 { cur_inst_key } else { cur_db_key } };
        st.remote_assistance = self
            .remote_assistance
            .into_iter()
            .map(|((tag, sid), v)| ((untag(tag), sid), v))
            .collect();
        st.pending_wubble_probes = self
            .pending_wubble_probes
            .into_iter()
            .map(|(tag, v)| (untag(tag), v))
            .collect();
        st.active_wubble_debuffs = self
            .active_wubble_debuffs
            .into_iter()
            .map(|((tag, sid), v)| ((untag(tag), sid), v))
            .collect();
        st.active_interdiction_probes = self
            .active_interdiction_probes
            .into_iter()
            .map(|((tag, pid), v)| ((untag(tag), pid), v))
            .collect();
        st.launcher_last_fire = self
            .launcher_last_fire
            .into_iter()
            .map(|((tag, mid), v)| ((untag(tag), mid), v))
            .collect();
        st.system_sim_time_s = self
            .system_sim_time_s
            .into_iter()
            .map(|((tag, sys), v)| ((untag(tag), sys), v))
            .collect();
        let mut legacy = LegacyTacticalDurableEntries::default();
        for ((_, sid), v) in self.lance_caster_lock_until {
            collect_max_u64(&mut legacy.lance_caster_lock_until, sid, v);
        }
        for ((_, sid), v) in self.lance_victim_debuff_until {
            collect_max_u64(&mut legacy.lance_victim_debuff_until, sid, v);
        }
        for ((_, sid), v) in self.tsb_self_penalty_until {
            collect_max_u64(&mut legacy.tsb_self_penalty_until, sid, v);
        }
        legacy
    }
}

#[derive(Default)]
struct LegacyTacticalDurableEntries {
    lance_caster_lock_until: HashMap<i64, u64>,
    lance_victim_debuff_until: HashMap<i64, u64>,
    tsb_self_penalty_until: HashMap<i64, u64>,
}

impl LegacyTacticalDurableEntries {
    fn is_empty(&self) -> bool {
        self.lance_caster_lock_until.is_empty()
            && self.lance_victim_debuff_until.is_empty()
            && self.tsb_self_penalty_until.is_empty()
    }
}

fn collect_max_u64(map: &mut HashMap<i64, u64>, ship_id: i64, until_unix: u64) {
    map.entry(ship_id)
        .and_modify(|cur| {
            if until_unix > *cur {
                *cur = until_unix;
            }
        })
        .or_insert(until_unix);
}

fn map_has_dominated_upgrade(existing: &HashMap<i64, u64>, incoming: &HashMap<i64, u64>) -> bool {
    incoming
        .iter()
        .any(|(&sid, &v)| existing.get(&sid).is_none_or(|&cur| v > cur))
}

fn merge_max_u64_map(dst: &mut HashMap<i64, u64>, src: HashMap<i64, u64>) {
    for (ship_id, until_unix) in src {
        collect_max_u64(dst, ship_id, until_unix);
    }
}

/// One-way legacy migration (`#1541` Track G2b-3/G2b-4): merges legacy durable tactical rows
/// (`lcl|...`, `lvd|...`, `tsb|...`) and legacy payload entries into `WorldStateSnapshot.combat_crimewatch_state`
/// via `max(existing, v)`, strictly outside any `ctx.tactical_state` lock (`RANK_WORLD_INNER` 40 < `RANK_LEAF` 60)
/// and performing zero DB writes when empty or already dominated.
fn merge_legacy_tactical_durable_into_db(
    db: &eve_db::WorldRepository,
    entries: LegacyTacticalDurableEntries,
    can_write_db: bool,
) {
    if entries.is_empty() || !can_write_db {
        return;
    }
    let needs_write = db.with_snapshot(|snap| {
        let cw = &snap.combat_crimewatch_state;
        map_has_dominated_upgrade(&cw.lance_caster_lock_until, &entries.lance_caster_lock_until)
            || map_has_dominated_upgrade(
                &cw.lance_victim_debuff_until,
                &entries.lance_victim_debuff_until,
            )
            || map_has_dominated_upgrade(&cw.tsb_self_penalty_until, &entries.tsb_self_penalty_until)
    });
    if !needs_write {
        return;
    }
    db.with_mut_snapshot(|snap| {
        let cw = snap.combat_crimewatch_state.as_inner_mut();
        merge_max_u64_map(&mut cw.lance_caster_lock_until, entries.lance_caster_lock_until);
        merge_max_u64_map(
            &mut cw.lance_victim_debuff_until,
            entries.lance_victim_debuff_until,
        );
        merge_max_u64_map(&mut cw.tsb_self_penalty_until, entries.tsb_self_penalty_until);
    });
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct PersistedFleetExtrasPayload {
    fleet_names: HashMap<i64, String>,
    max_sizes: HashMap<i64, u16>,
    auto_join_squads: HashMap<i64, Option<i64>>,
    invite_details: Vec<((i64, i64), (i64, i64, FleetRole, i64))>,
    invite_timestamps: Vec<((i64, i64), u64)>,
    disconnected_members: HashMap<i64, u64>,
    adverts: HashMap<i64, FleetAdvertState>,
    target_tags: HashMap<i64, HashMap<i64, String>>,
    wing_squad_hierarchy: HashMap<i64, (Vec<i64>, Vec<i64>)>,
    saved_setups: HashMap<i64, HashMap<String, FleetSetupRecord>>,
    stored_formations: HashMap<i64, (i64, f64, f64)>,
    broadcast_filters: HashMap<i64, HashSet<String>>,
    last_broadcast: HashMap<i64, (u64, String, i64, i64, Option<i64>)>,
    broadcast_history: HashMap<i64, Vec<(u64, i64, String, i64, Option<i64>, Option<i64>)>>,
}

impl PersistedFleetExtrasPayload {
    fn from_ctx(ctx: &ServerContext) -> Self {
        let ex = ctx.fleet_extras.lock().unwrap_or_else(|e| e.into_inner());
        let mut invite_details: Vec<_> =
            ex.invite_details.iter().map(|(&k, v)| (k, v.clone())).collect();
        invite_details.sort_by_key(|(k, _)| *k);
        let mut invite_timestamps: Vec<_> =
            ex.invite_timestamps.iter().map(|(&k, &v)| (k, v)).collect();
        invite_timestamps.sort_by_key(|(k, _)| *k);
        Self {
            fleet_names: ex.fleet_names.clone(),
            max_sizes: ex.max_sizes.clone(),
            auto_join_squads: ex.auto_join_squads.clone(),
            invite_details,
            invite_timestamps,
            disconnected_members: ex.disconnected_members.clone(),
            adverts: ex.adverts.clone(),
            target_tags: ex.target_tags.clone(),
            wing_squad_hierarchy: ex.wing_squad_hierarchy.clone(),
            saved_setups: ex.saved_setups.clone(),
            stored_formations: ex.stored_formations.clone(),
            broadcast_filters: ex.broadcast_filters.clone(),
            last_broadcast: ex.last_broadcast.clone(),
            broadcast_history: ex.broadcast_history.clone(),
        }
    }

    fn is_empty(&self) -> bool {
        self.fleet_names.is_empty()
            && self.max_sizes.is_empty()
            && self.auto_join_squads.is_empty()
            && self.invite_details.is_empty()
            && self.invite_timestamps.is_empty()
            && self.disconnected_members.is_empty()
            && self.adverts.is_empty()
            && self.target_tags.is_empty()
            && self.wing_squad_hierarchy.is_empty()
            && self.saved_setups.is_empty()
            && self.stored_formations.is_empty()
            && self.broadcast_filters.is_empty()
            && self.last_broadcast.is_empty()
            && self.broadcast_history.is_empty()
    }

    fn apply_to_ctx(self, ctx: &ServerContext) {
        let mut ex = ctx.fleet_extras.lock().unwrap_or_else(|e| e.into_inner());
        ex.fleet_names = self.fleet_names;
        ex.max_sizes = self.max_sizes;
        ex.auto_join_squads = self.auto_join_squads;
        ex.invite_details = self.invite_details.into_iter().collect();
        ex.invite_timestamps = self.invite_timestamps.into_iter().collect();
        ex.disconnected_members = self.disconnected_members;
        ex.adverts = self.adverts;
        ex.target_tags = self.target_tags;
        ex.wing_squad_hierarchy = self.wing_squad_hierarchy;
        ex.saved_setups = self.saved_setups;
        ex.stored_formations = self.stored_formations;
        ex.broadcast_filters = self.broadcast_filters;
        ex.last_broadcast = self.last_broadcast;
        ex.broadcast_history = self.broadcast_history;
    }
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct PersistedSocialExtrasRuntimePayload {
    delayed_local_speakers: HashMap<String, HashSet<i64>>,
    active_probes: HashMap<i64, Vec<(i64, [f64; 3], f64, u64)>>,
    forced_session_corp_sync: HashMap<i64, (i64, Option<i64>, u64)>,
    forced_session_ship_sync: HashMap<i64, (i64, Option<i64>)>,
}

impl PersistedSocialExtrasRuntimePayload {
    fn from_ctx(ctx: &ServerContext) -> Self {
        let se = ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner());
        Self {
            delayed_local_speakers: se.delayed_local_speakers.clone(),
            active_probes: se.active_probes.clone(),
            forced_session_corp_sync: se.forced_session_corp_sync.clone(),
            forced_session_ship_sync: se.forced_session_ship_sync.clone(),
        }
    }

    fn is_empty(&self) -> bool {
        self.delayed_local_speakers.is_empty()
            && self.active_probes.is_empty()
            && self.forced_session_corp_sync.is_empty()
            && self.forced_session_ship_sync.is_empty()
    }

    fn apply_to_ctx(self, ctx: &ServerContext) {
        let mut se = ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner());
        se.delayed_local_speakers = self.delayed_local_speakers;
        se.active_probes = self.active_probes;
        se.forced_session_corp_sync = self.forced_session_corp_sync;
        se.forced_session_ship_sync = self.forced_session_ship_sync;
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
enum DurableTacticalEntry {
    LanceCasterLockUntil(u8, i64, u64),
    LanceVictimDebuffUntil(u8, i64, u64),
    TsbSelfPenaltyUntil(u8, i64, u64),
}

fn resolve_entity_solar_system_id(snap: &WorldStateSnapshot, entity_id: i64) -> u32 {
    if let Some(st) = snap.structures.get(&entity_id) {
        return st.solar_system_id;
    }
    if let Some(item) = snap.items.get(&entity_id) {
        if let Ok(loc_u32) = u32::try_from(item.location_id) {
            if (30_000_000..40_000_000).contains(&loc_u32) {
                return loc_u32;
            }
        }
        if let Some(st) = snap.structures.get(&item.location_id) {
            return st.solar_system_id;
        }
    }
    CLUSTER_SYSTEM_ID
}

fn push_checked_row<T: serde::Serialize>(
    out: &mut Vec<(u32, String, String)>,
    system_id: u32,
    key: String,
    val: &T,
) {
    if let Ok(json) = to_checked_domain_json("system_checkpoint", &key, val) {
        out.push((system_id, key, json));
    }
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct PersistedServerRuntimeState {
    // Legacy monolith migration struct: keeps ONLY declared checkpointed fields; crash-lost
    // fields in legacy JSON payloads are intentionally skipped on deserialize.
    active_moon_probes: Vec<ActiveMoonSurveyProbe>,
    scanned_signatures: Vec<(i64, u32, i64)>,
    character_scan_progress: Vec<((i64, u32, String), f64)>,
    active_fighters: HashMap<i64, ActiveFighterSquadron>,
    fighter_cooldown_state: Vec<((i64, u32), u64)>,
    fighter_charge_state: Vec<((i64, u32), u32)>,
    fighter_tube_loads: Vec<((i64, usize), i64)>,
    module_reactivation_cooldowns: Vec<((i64, i64, u32), ModuleCooldownEntry)>,
    phased_asteroid_fields: HashMap<u32, PhasedAsteroidFieldState>,
    session_change_timers: HashMap<i64, SessionChangeTimerRecord>,
    map_landmarks: Vec<(i64, String, f64, f64, f64)>,
    tactical_state: PersistedTacticalModulesPayload,
    fleet_extras_runtime: PersistedFleetExtrasPayload,
    social_extras_runtime: PersistedSocialExtrasRuntimePayload,
}

impl PersistedServerRuntimeState {
    #[cfg(test)]
    fn from_ctx(ctx: &ServerContext) -> Self {
        let cur_db_key = db_ptr_key(&ctx.db);
        let cur_inst_key = ctx.instance_id as usize;
        let tactical_state = {
            let tac = ctx.tactical_state.read().unwrap_or_else(|e| e.into_inner());
            PersistedTacticalModulesPayload::from_state_for_keys(&tac, Some(cur_db_key), cur_inst_key)
        };
        let fleet_extras_runtime = PersistedFleetExtrasPayload::from_ctx(ctx);
        let social_extras_runtime = PersistedSocialExtrasRuntimePayload::from_ctx(ctx);

        let mut scanned_signatures: Vec<_> = ctx
            .scanned_signatures
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied()
            .collect();
        scanned_signatures.sort_unstable();
        let mut character_scan_progress: Vec<_> = ctx
            .character_scan_progress
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(k, &v)| (k.clone(), v))
            .collect();
        character_scan_progress.sort_by(|(a, _), (b, _)| a.cmp(b));

        let active_moon_probes = ctx
            .active_moon_probes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let phased_asteroid_fields = ctx
            .phased_asteroid_fields
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let map_landmarks = ctx
            .map_landmarks
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

        Self {
            active_moon_probes,
            scanned_signatures,
            character_scan_progress,
            active_fighters: HashMap::new(),
            fighter_cooldown_state: Vec::new(),
            fighter_charge_state: Vec::new(),
            fighter_tube_loads: Vec::new(),
            module_reactivation_cooldowns: Vec::new(),
            phased_asteroid_fields,
            session_change_timers: HashMap::new(),
            map_landmarks,
            tactical_state,
            fleet_extras_runtime,
            social_extras_runtime,
        }
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.active_moon_probes.is_empty()
            && self.scanned_signatures.is_empty()
            && self.character_scan_progress.is_empty()
            && self.active_fighters.is_empty()
            && self.fighter_cooldown_state.is_empty()
            && self.fighter_charge_state.is_empty()
            && self.fighter_tube_loads.is_empty()
            && self.phased_asteroid_fields.is_empty()
            && self.map_landmarks.is_empty()
            && self.tactical_state.is_empty()
            && self.fleet_extras_runtime.is_empty()
            && self.social_extras_runtime.is_empty()
    }

    fn apply_to_ctx(self, ctx: &ServerContext, can_write_db: bool) {
        // Legacy monolith migration keeps ONLY declared checkpointed kinds; crash-lost kinds
        // (`module_cycles`, `target_locks`, `active_warps`) are intentionally dropped.
        *ctx.active_moon_probes
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.active_moon_probes;
        *ctx.scanned_signatures
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.scanned_signatures.into_iter().collect();
        *ctx.character_scan_progress
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.character_scan_progress.into_iter().collect();
        if !self.active_fighters.is_empty()
            || !self.fighter_cooldown_state.is_empty()
            || !self.fighter_charge_state.is_empty()
            || !self.fighter_tube_loads.is_empty()
        {
            crate::system_actor::migrate_legacy_monolith_fighters_one_way(
                &ctx.db,
                Some(ctx.sde.as_ref()),
                &ctx.systems,
                self.active_fighters,
                self.fighter_cooldown_state,
                self.fighter_charge_state,
                self.fighter_tube_loads,
            );
        }
        crate::system_actor::import_legacy_monolith_module_cooldowns_in_db(
            &ctx.systems,
            &ctx.db,
            ctx.now_unix_secs() as f64,
            self.module_reactivation_cooldowns,
        );
        ctx.systems.rehydrate_active_system_cooldowns_from_db();
        *ctx.phased_asteroid_fields
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.phased_asteroid_fields;
        if !self.session_change_timers.is_empty() {
            ctx.db.with_mut_snapshot(|s| {
                for (cid, rec) in self.session_change_timers {
                    s.sp6_lifecycle
                        .session_change_timers
                        .entry(cid)
                        .or_insert(rec);
                }
            });
        }
        *ctx.map_landmarks
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.map_landmarks;

        let cur_db_key = db_ptr_key(&ctx.db);
        let cur_inst_key = ctx.instance_id as usize;
        let legacy_durable = {
            let mut tac = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
            self.tactical_state
                .apply_to_state(&mut tac, cur_db_key, cur_inst_key)
        };
        merge_legacy_tactical_durable_into_db(&ctx.db, legacy_durable, can_write_db);

        self.fleet_extras_runtime.apply_to_ctx(ctx);
        self.social_extras_runtime.apply_to_ctx(ctx);
        clear_crash_lost_runtime_maps(ctx);
    }
}

fn clear_crash_lost_runtime_maps(ctx: &ServerContext) {
    ctx.mining_cycles
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    ctx.systems.clear_all_ewar_state_sync();
    {
        let mut tac = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
        tac.remote_assistance.clear();
        tac.pending_wubble_probes.clear();
        tac.active_wubble_debuffs.clear();
        tac.active_interdiction_probes.clear();
        tac.launcher_last_fire.clear();
    }
    ctx.systems.clear_all_external_active_modules();
    if ctx.db.with_snapshot(|snap| {
        !snap.sov_runtime.entosis_active_links.is_empty()
            || !snap.sov_runtime.entosis_target_cycles.is_empty()
    }) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.sov_runtime.entosis_active_links.clear();
            snap.sov_runtime.entosis_target_cycles.clear();
        });
    }
}

fn collect_checkpoint_batch_from_ctx(ctx: &ServerContext) -> Vec<(&'static str, Vec<(u32, String, String)>)> {
    let cur_db_key = db_ptr_key(&ctx.db);
    let cur_inst_key = ctx.instance_id as usize;
    let keep_tac_key = |k: usize| k == cur_db_key || k == cur_inst_key;
    let tac_tag = |k: usize| u8::from(k == cur_inst_key);

    let moon_probes_raw = ctx
        .active_moon_probes
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let mut scanned_signatures_raw: Vec<_> = ctx
        .scanned_signatures
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .copied()
        .collect();
    scanned_signatures_raw.sort_unstable();
    let mut scan_progress_raw: Vec<_> = ctx
        .character_scan_progress
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(k, &v)| (k.clone(), v))
        .collect();
    scan_progress_raw.sort_by(|(a, _), (b, _)| a.cmp(b));
    let phased_fields_raw = ctx
        .phased_asteroid_fields
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let landmarks_raw = ctx
        .map_landmarks
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let sim_times_raw = {
        let tac = ctx.tactical_state.read().unwrap_or_else(|e| e.into_inner());
        tac.system_sim_time_s
            .iter()
            .filter(|(&(k, _), _)| keep_tac_key(k))
            .map(|(&(k, sys), &val)| ((tac_tag(k), sys), val))
            .collect::<Vec<_>>()
    };
    let fleet_extras = PersistedFleetExtrasPayload::from_ctx(ctx);
    let social_extras = PersistedSocialExtrasRuntimePayload::from_ctx(ctx);

    let landmark_systems: Vec<u32> = ctx.db.with_snapshot(|snap| {
        landmarks_raw
            .iter()
            .map(|(item_id, _, _, _, _)| resolve_entity_solar_system_id(snap, *item_id))
            .collect()
    });

    let mut moon_rows = Vec::with_capacity(moon_probes_raw.len());
    for probe in &moon_probes_raw {
        push_checked_row(
            &mut moon_rows,
            probe.solar_system_id,
            probe.probe_id.to_string(),
            probe,
        );
    }

    let mut sig_rows = Vec::with_capacity(scanned_signatures_raw.len());
    for entry in &scanned_signatures_raw {
        push_checked_row(
            &mut sig_rows,
            entry.1,
            format!("{}|{}", entry.0, entry.2),
            entry,
        );
    }

    let mut prog_rows = Vec::with_capacity(scan_progress_raw.len());
    for ((char_id, sys_id, sig_key), val) in &scan_progress_raw {
        push_checked_row(
            &mut prog_rows,
            *sys_id,
            format!("{char_id}|{sig_key}"),
            &((*char_id, *sys_id, sig_key.clone()), *val),
        );
    }

    let mut phased_rows = Vec::with_capacity(phased_fields_raw.len());
    for (&sys_id, st) in &phased_fields_raw {
        push_checked_row(&mut phased_rows, sys_id, "field".to_string(), st);
    }

    let mut landmark_rows = Vec::with_capacity(landmarks_raw.len());
    for (idx, (entry, &sys_id)) in landmarks_raw.iter().zip(landmark_systems.iter()).enumerate() {
        push_checked_row(
            &mut landmark_rows,
            sys_id,
            format!("{}|{idx}", entry.0),
            entry,
        );
    }

    let mut sim_rows = Vec::with_capacity(sim_times_raw.len());
    for ((tag, sys_id), sim_s) in &sim_times_raw {
        push_checked_row(&mut sim_rows, *sys_id, tag.to_string(), &((*tag, *sys_id), *sim_s));
    }

    let durable_tac_rows = Vec::new();

    let mut fleet_rows = Vec::new();
    if !fleet_extras.is_empty() {
        push_checked_row(
            &mut fleet_rows,
            CLUSTER_SYSTEM_ID,
            "state".to_string(),
            &fleet_extras,
        );
    }

    let mut social_rows = Vec::new();
    if !social_extras.is_empty() {
        push_checked_row(
            &mut social_rows,
            CLUSTER_SYSTEM_ID,
            "state".to_string(),
            &social_extras,
        );
    }

    vec![
        (KIND_MOON_PROBES, moon_rows),
        (KIND_SCAN_SIGNATURES, sig_rows),
        (KIND_SCAN_PROGRESS, prog_rows),
        (KIND_PHASED_ASTEROID_FIELDS, phased_rows),
        (KIND_MAP_LANDMARKS, landmark_rows),
        (KIND_SYSTEM_SIM_TIME, sim_rows),
        (KIND_TACTICAL_DURABLE, durable_tac_rows),
        (KIND_FLEET_EXTRAS, fleet_rows),
        (KIND_SOCIAL_EXTRAS, social_rows),
    ]
}

enum CheckpointLoadOutcome {
    Loaded,
    Empty,
    Error(String),
}

fn load_checkpoints_into_ctx(ctx: &ServerContext, can_write_db: bool) -> CheckpointLoadOutcome {
    let loaded = ctx.db.with_sqlite_conn(|conn| -> Result<Option<_>, String> {
        ensure_system_checkpoint_schema(conn)?;
        let _ = purge_crash_lost_checkpoints(conn);
        if !has_any_system_checkpoints(conn)? {
            return Ok(None);
        }
        let moon_probes =
            load_all_checkpoints_for_kind::<ActiveMoonSurveyProbe>(conn, KIND_MOON_PROBES)?;
        let sigs =
            load_all_checkpoints_for_kind::<(i64, u32, i64)>(conn, KIND_SCAN_SIGNATURES)?;
        let prog =
            load_all_checkpoints_for_kind::<((i64, u32, String), f64)>(conn, KIND_SCAN_PROGRESS)?;
        let phased = load_all_checkpoints_for_kind::<PhasedAsteroidFieldState>(
            conn,
            KIND_PHASED_ASTEROID_FIELDS,
        )?;
        let landmarks =
            load_all_checkpoints_for_kind::<(i64, String, f64, f64, f64)>(conn, KIND_MAP_LANDMARKS)?;
        let sim_times =
            load_all_checkpoints_for_kind::<((u8, u32), f64)>(conn, KIND_SYSTEM_SIM_TIME)?;
        let durable_tac_raw =
            load_all_checkpoints_for_kind::<serde_json::Value>(conn, KIND_TACTICAL_DURABLE)?;
        let mut durable_tac = Vec::with_capacity(durable_tac_raw.len());
        for (sys_id, key, raw_val) in durable_tac_raw {
            // Skip legacy `cvb|...` rows written before `#1541` Track G2b-1.
            if key.starts_with("cvb|") {
                continue;
            }
            let entry: DurableTacticalEntry = serde_json::from_value(raw_val)
                .map_err(|e| format!("decode durable tactical entry '{key}': {e}"))?;
            durable_tac.push((sys_id, key, entry));
        }
        let fleet_extras =
            load_all_checkpoints_for_kind::<PersistedFleetExtrasPayload>(conn, KIND_FLEET_EXTRAS)?;
        let social_extras = load_all_checkpoints_for_kind::<PersistedSocialExtrasRuntimePayload>(
            conn,
            KIND_SOCIAL_EXTRAS,
        )?;
        Ok(Some((
            moon_probes,
            sigs,
            prog,
            phased,
            landmarks,
            sim_times,
            durable_tac,
            fleet_extras,
            social_extras,
        )))
    });

    let (
        moon_probes,
        sigs,
        prog,
        phased,
        landmarks,
        sim_times,
        durable_tac,
        fleet_extras,
        social_extras,
    ) = match loaded {
        Ok(Some(data)) => data,
        Ok(None) => return CheckpointLoadOutcome::Empty,
        Err(err) => return CheckpointLoadOutcome::Error(err),
    };

    *ctx.active_moon_probes
        .write()
        .unwrap_or_else(|e| e.into_inner()) = moon_probes.into_iter().map(|(_, _, v)| v).collect();
    *ctx.scanned_signatures
        .write()
        .unwrap_or_else(|e| e.into_inner()) = sigs.into_iter().map(|(_, _, v)| v).collect();
    *ctx.character_scan_progress
        .write()
        .unwrap_or_else(|e| e.into_inner()) = prog.into_iter().map(|(_, _, (k, v))| (k, v)).collect();
    *ctx.phased_asteroid_fields
        .write()
        .unwrap_or_else(|e| e.into_inner()) = phased.into_iter().map(|(sys, _, v)| (sys, v)).collect();
    *ctx.map_landmarks
        .write()
        .unwrap_or_else(|e| e.into_inner()) = landmarks.into_iter().map(|(_, _, v)| v).collect();

    let cur_db_key = db_ptr_key(&ctx.db);
    let cur_inst_key = ctx.instance_id as usize;
    let untag = |tag: u8| -> usize {
        if tag == 1 {
            cur_inst_key
        } else {
            cur_db_key
        }
    };
    let mut legacy_durable = LegacyTacticalDurableEntries::default();
    for (_, _, entry) in durable_tac {
        match entry {
            DurableTacticalEntry::LanceCasterLockUntil(_tag, sid, v) => {
                collect_max_u64(&mut legacy_durable.lance_caster_lock_until, sid, v);
            }
            DurableTacticalEntry::LanceVictimDebuffUntil(_tag, sid, v) => {
                collect_max_u64(&mut legacy_durable.lance_victim_debuff_until, sid, v);
            }
            DurableTacticalEntry::TsbSelfPenaltyUntil(_tag, sid, v) => {
                collect_max_u64(&mut legacy_durable.tsb_self_penalty_until, sid, v);
            }
        }
    }
    {
        let mut tac = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
        tac.system_sim_time_s.clear();
        for (_, _, ((tag, sys), sim_s)) in sim_times {
            tac.system_sim_time_s.insert((untag(tag), sys), sim_s);
        }
    }
    merge_legacy_tactical_durable_into_db(&ctx.db, legacy_durable, can_write_db);

    if let Some((_, _, payload)) = fleet_extras.into_iter().next() {
        payload.apply_to_ctx(ctx);
    } else {
        PersistedFleetExtrasPayload::default().apply_to_ctx(ctx);
    }
    if let Some((_, _, payload)) = social_extras.into_iter().next() {
        payload.apply_to_ctx(ctx);
    } else {
        PersistedSocialExtrasRuntimePayload::default().apply_to_ctx(ctx);
    }
    clear_crash_lost_runtime_maps(ctx);
    CheckpointLoadOutcome::Loaded
}

impl ServerContext {
    /// Hydrates all gameplay-affecting `ServerContext` runtime maps from `WorldRepository` (`#592`).
    pub fn hydrate_server_runtime_from_db(&self) {
        let can_write_db = !self.db.is_write_locked_by_current_thread();
        if can_write_db
            && self
                .db
                .with_snapshot(|snap| snap.owner_notes.values().any(|n| n.character_id == -73))
        {
            self.db.with_mut_snapshot(|snap| {
                let online: std::collections::HashSet<i64> = snap
                    .sessions
                    .values()
                    .filter_map(|s| s.character_id)
                    .collect();
                snap.owner_notes.retain(|_, n| {
                    if n.character_id != -73 {
                        return true;
                    }
                    serde_json::from_str::<crate::services::wallet_services_ext::TradeSessionState>(
                        &n.note,
                    )
                    .is_ok_and(|t| {
                        !t.completed
                            && !t.cancelled
                            && online.contains(&t.char_a)
                            && online.contains(&t.char_b)
                    })
                });
            });
        }
        let (blob_opt, global_cfg, notifications, persisted_probes) = self.db.with_snapshot(|snap| {
            let sp6_state = crate::sp6_lifecycle::load_sp6_state_from_snap(snap);
            let blob = snap
                .cluster_and_runtime
                .server_runtime_json
                .clone()
                .or_else(|| sp6_state.server_runtime_json.clone())
                .or_else(|| {
                    snap.service_state_blobs
                        .get(SERVER_RUNTIME_STATE_BLOB_KEY)
                        .cloned()
                });
            let gcfg: HashMap<String, eve_proto::PyRep> = snap
                .service_state_blobs
                .get("macho_global_config")
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            let mut notifs: Vec<super::cache_notifications::NotificationRecord> = snap
                .owner_notes
                .values()
                .filter(|n| n.character_id == -79)
                .filter_map(|n| serde_json::from_str(&n.note).ok())
                .collect();
            notifs.sort_by_key(|r| r.notification_id);
            let probes: HashMap<i64, ActiveScannerProbe> = sp6_state
                .active_scanner_probes
                .into_iter()
                .filter(|(_, p)| p.state >= 0)
                .map(|(id, mut p)| {
                    // Cluster restarts disconnect launched probes (`INACTIVE = 0`) and cancel
                    // in-flight warp timers so ReconnectToLostProbes can reclaim them.
                    if p.state > 0 {
                        p.state = 0;
                    }
                    p.warp_end_ft = None;
                    (id, p)
                })
                .collect();
            (blob, gcfg, notifs, probes)
        });
        *self
            .global_config_values
            .write()
            .unwrap_or_else(|e| e.into_inner()) = global_cfg;
        *self
            .notification_records
            .write()
            .unwrap_or_else(|e| e.into_inner()) = notifications;
        *self
            .active_scanner_probes
            .write()
            .unwrap_or_else(|e| e.into_inner()) = persisted_probes;

        let state = blob_opt
            .and_then(|s| from_checked_domain_json::<PersistedServerRuntimeState>(&s).ok())
            .or_else(|| {
                self.db
                    .with_sqlite_conn(|conn| {
                        eve_db::sqlite_store::system_checkpoint::read_legacy_server_runtime_blob(conn)
                    })
                    .ok()
                    .flatten()
                    .and_then(|s| serde_json::from_str::<PersistedServerRuntimeState>(&s).ok())
            });

        match load_checkpoints_into_ctx(self, can_write_db) {
            CheckpointLoadOutcome::Loaded => {
                crate::system_actor::reload_live_fighters_from_db(
                    &self.systems,
                    &self.db,
                    Some(self.sde.as_ref()),
                );
                self.systems.rehydrate_active_system_cooldowns_from_db();
                crate::system_actor::migrate_and_prune_module_cooldowns_in_db(
                    &self.systems,
                    &self.db,
                    self.now_unix_secs() as f64,
                );
                self.retire_legacy_runtime_monolith_in_db();
                return;
            }
            CheckpointLoadOutcome::Error(err) => {
                tracing::error!(
                    "hydrate_server_runtime_from_db aborted due to system_checkpoint read error (preserving live runtime state): {err}"
                );
                return;
            }
            CheckpointLoadOutcome::Empty => {}
        }

        if let Some(legacy_state) = state {
            legacy_state.apply_to_ctx(self, can_write_db);
            self.persist_server_runtime_to_db();
        } else {
            PersistedServerRuntimeState::default().apply_to_ctx(self, can_write_db);
            crate::system_actor::reload_live_fighters_from_db(
                &self.systems,
                &self.db,
                Some(self.sde.as_ref()),
            );
            self.retire_legacy_runtime_monolith_in_db();
        }
    }

    fn retire_legacy_runtime_monolith_in_db(&self) {
        if self.db.is_write_locked_by_current_thread() {
            return;
        }
        let has_legacy = self.db.with_snapshot(|s| {
            s.cluster_and_runtime.server_runtime_json.is_some()
                || s.sp6_lifecycle.server_runtime_json.is_some()
                || s.service_state_blobs
                    .contains_key(SERVER_RUNTIME_STATE_BLOB_KEY)
        });
        if has_legacy {
            let _ = self.db.with_mut_snapshot(|snap| {
                snap.cluster_and_runtime.server_runtime_json = None;
                snap.sp6_lifecycle.server_runtime_json = None;
                snap.service_state_blobs
                    .remove(SERVER_RUNTIME_STATE_BLOB_KEY);
            });
        }
        let _ = self
            .db
            .with_sqlite_conn(|conn| retire_legacy_server_runtime_blobs(conn));
    }

    /// Persists all declared `Checkpointed` `ServerContext` runtime maps into per-system
    /// SQLite `system_checkpoint(system_id, kind, key, value)` rows and retires any legacy
    /// `server_runtime_state` monolith (`#592`, `#920`). Crash-lost kinds (`module_cycles`,
    /// `target_locks`, `active_warps`, `npcs`) are intentionally omitted.
    pub fn persist_server_runtime_to_db(&self) {
        if self.db.is_write_locked_by_current_thread() {
            return;
        }
        // Keep `sp6_lifecycle.active_scanner_probes` synchronized even when callers invoke
        // `persist_server_runtime_to_db`/`sync_runtime_to_db` directly without `sync_scanner_probes_to_db`,
        // preserving the `db` -> `active_scanner_probes` lock order.
        let probes_differ = self.db.with_snapshot(|snap| {
            let map = self
                .active_scanner_probes
                .read()
                .unwrap_or_else(|e| e.into_inner());
            let persisted = &snap.sp6_lifecycle.active_scanner_probes;
            let live_count = map.values().filter(|p| p.state >= 0).count();
            if persisted.len() != live_count {
                return true;
            }
            map.iter()
                .filter(|(_, p)| p.state >= 0)
                .any(|(id, p)| persisted.get(id) != Some(p))
        });
        if probes_differ {
            crate::sp6_lifecycle::with_mut_sp6_state(&self.db, |st| {
                let map = self
                    .active_scanner_probes
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
                st.active_scanner_probes = map
                    .iter()
                    .filter(|(_, p)| p.state >= 0)
                    .map(|(&id, p)| (id, p.clone()))
                    .collect();
            });
        }

        let batch = collect_checkpoint_batch_from_ctx(self);
        let now_unix = current_unix_secs();
        let _ = self.db.with_sqlite_conn(|conn| {
            let borrowed: Vec<(&str, Vec<(u32, String, String)>)> = batch
                .iter()
                .map(|(k, rows)| (*k, rows.clone()))
                .collect();
            replace_checkpoint_kinds_batch(conn, &borrowed, now_unix)
        });
        self.systems.flush_all_module_cooldown_checkpoints_to_db();
        self.retire_legacy_runtime_monolith_in_db();
    }

    /// Flushes all gameplay-affecting runtime state across `ServerContext`, `CombatRuntimeState`,
    /// social extras, and `QuasarGateway` into SQLite (`#592`).
    pub fn sync_runtime_to_db(&self) {
        self.systems.flush_all_condition_checkpoints_to_db();
        self.systems.flush_all_fighter_checkpoints_to_db();
        self.persist_server_runtime_to_db();
        flush_combat_runtime_to_db(self);
        let _ = persist_social_state(self);
        self.quasar_gateway.sync_all_to_db();
    }

    /// Rehydrates all gameplay-affecting runtime state across `ServerContext`, `CombatRuntimeState`,
    /// social extras, and `QuasarGateway` from SQLite (`#592`).
    pub fn rehydrate_runtime_from_db(&self) {
        hydrate_social_state_from_db(self);
        self.hydrate_server_runtime_from_db();
        rehydrate_combat_runtime_from_db_forced(self);
        self.quasar_gateway.rehydrate_all_from_db();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eve_db::{OwnerNoteRecord, WorldRepository};
    use glam::DVec3;
    use std::sync::Arc;

    fn test_ctx() -> ServerContext {
        ServerContext::with_repository(Arc::new(WorldRepository::new_empty()))
    }

    #[test]
    fn intern_ewar_kind_returns_static_exact_matches_and_default() {
        for expected in [
            "warpScramblerMWD",
            "warpScrambler",
            "focusedWarpScrambler",
            "webify",
            "electronic",
            "ewRemoteSensorDamp",
            "ewGuidanceDisrupt",
            "ewTrackingDisrupt",
            "ewTargetPaint",
            "ewEnergyNeut",
            "ewEnergyVampire",
        ] {
            let owned = String::from(expected);
            assert_eq!(intern_ewar_kind(&owned), expected);
        }
        assert_eq!(intern_ewar_kind("unknownCustomEwar"), "electronic");
        assert_eq!(intern_ewar_kind(""), "electronic");
    }

    #[test]
    fn persisted_tactical_payload_tagging_remapping_sorting_and_is_empty() {
        let mut st = TacticalModulesState::default();
        let db_k = 1_111_usize;
        let inst_k = 2_222_usize;

        assert!(PersistedTacticalModulesPayload::from_state(&st, inst_k).is_empty());

        // Populate entries keyed by both `db_k` (tag 0) and `inst_k` (tag 1) in reverse order to test sorting.
        st.remote_assistance.insert(
            (inst_k, 20),
            vec![ProjectedRemoteAssistance {
                source_ship_id: 1,
                module_id: 2,
                module_type_id: 3,
                max_target_range_bonus_pct: 10.0,
                scan_resolution_bonus_pct: 20.0,
                sensor_strength_bonus_pct: 0.0,
                optimal_range_bonus_pct: 0.0,
                falloff_bonus_pct: 0.0,
                tracking_speed_bonus_pct: 0.0,
            }],
        );
        st.remote_assistance.insert((db_k, 10), Vec::new());
        st.pending_wubble_probes.push((
            inst_k,
            PendingWubbleProbe {
                probe_id: 90,
                solar_system_id: 30_000_142,
                position: DVec3::ZERO,
                radius_m: 10_000.0,
                remaining_arming_s: 3.0,
                duration_s: 30.0,
                velocity_multiplier: 0.5,
                detonated: false,
            },
        ));
        st.active_wubble_debuffs.insert(
            (db_k, 100),
            ActiveWubbleDebuff {
                remaining_s: 15.0,
                velocity_multiplier: 0.5,
            },
        );
        st.active_interdiction_probes.insert(
            (db_k, 130),
            ActiveInterdictionProbe {
                probe_id: 130,
                probe_type_id: 22_778,
                solar_system_id: 30_000_142,
                remaining_s: 60.0,
                is_wubble: false,
            },
        );
        st.launcher_last_fire.insert(
            (inst_k, 140),
            LauncherFireStamp {
                fired_unix: 500,
                fired_tick: 12,
                fired_sim_s: 12.0,
            },
        );
        st.system_sim_time_s.insert((db_k, 30_000_142), 42.5);

        let payload = PersistedTacticalModulesPayload::from_state(&st, inst_k);
        assert!(!payload.is_empty());
        // Tag `0` (`db_k`) sorts before tag `1` (`inst_k`)
        assert_eq!(payload.remote_assistance[0].0, (0, 10));
        assert_eq!(payload.remote_assistance[1].0, (1, 20));

        // Remap into a fresh `new_db_k` and `new_inst_k`
        let new_db_k = 8_888_usize;
        let new_inst_k = 9_999_usize;
        let mut restored = TacticalModulesState::default();
        let legacy_durable = payload.apply_to_state(&mut restored, new_db_k, new_inst_k);
        assert!(legacy_durable.is_empty());
        assert!(restored.remote_assistance.contains_key(&(new_db_k, 10)));
        assert!(restored.remote_assistance.contains_key(&(new_inst_k, 20)));
        assert_eq!(restored.pending_wubble_probes[0].0, new_inst_k);
        assert!(restored.active_wubble_debuffs.contains_key(&(new_db_k, 100)));
        assert!(restored.active_interdiction_probes.contains_key(&(new_db_k, 130)));
        assert_eq!(restored.launcher_last_fire[&(new_inst_k, 140)].fired_unix, 500);
        assert_eq!(restored.system_sim_time_s[&(new_db_k, 30_000_142)], 42.5);
    }

    #[test]
    fn persisted_server_runtime_state_is_empty_and_write_lock_guard() {
        let ctx = test_ctx();
        assert!(PersistedServerRuntimeState::from_ctx(&ctx).is_empty());

        // 1. Re-entrancy guard: calling `persist_server_runtime_to_db` while holding `with_mut_snapshot`
        //    must return immediately without deadlocking or writing `system_checkpoint`.
        ctx.map_landmarks
            .write()
            .unwrap()
            .push((1, "Landmark".into(), 1.0, 2.0, 3.0));
        assert!(!PersistedServerRuntimeState::from_ctx(&ctx).is_empty());

        ctx.db.with_mut_snapshot(|snap| {
            ctx.persist_server_runtime_to_db();
            assert!(snap.cluster_and_runtime.server_runtime_json.is_none());
        });

        // 2. Outside the write lock, `persist_server_runtime_to_db` writes per-system
        //    `system_checkpoint` rows and clears legacy `server_runtime_json` + `service_state_blobs`.
        ctx.db.with_mut_snapshot(|snap| {
            snap.sp6_lifecycle.server_runtime_json = Some("{\"legacy\":true}".into());
            snap.service_state_blobs
                .insert(SERVER_RUNTIME_STATE_BLOB_KEY.into(), "{\"legacy\":true}".into());
        });
        ctx.persist_server_runtime_to_db();
        ctx.db.with_snapshot(|snap| {
            assert!(snap.cluster_and_runtime.server_runtime_json.is_none());
            assert!(snap.sp6_lifecycle.server_runtime_json.is_none());
            assert!(!snap
                .service_state_blobs
                .contains_key(SERVER_RUNTIME_STATE_BLOB_KEY));
        });
        let has_rows = ctx
            .db
            .with_sqlite_conn(|conn| has_any_system_checkpoints(conn))
            .expect("check system_checkpoint rows");
        assert!(has_rows);

        // 3. Clearing all checkpointed runtime fields removes their `system_checkpoint` rows.
        ctx.map_landmarks.write().unwrap().clear();
        assert!(PersistedServerRuntimeState::from_ctx(&ctx).is_empty());
        ctx.persist_server_runtime_to_db();
        let has_rows_after_clear = ctx
            .db
            .with_sqlite_conn(|conn| has_any_system_checkpoints(conn))
            .expect("check system_checkpoint empty");
        assert!(!has_rows_after_clear);
    }

    #[test]
    fn hydrate_server_runtime_resets_probes_sorts_notifications_and_ignores_corrupt_json() {
        let ctx = test_ctx();

        // Seed:
        // - Active scanner probes: one `state = 2` (active -> must reset to `state = 0` and `warp_end_ft = None`),
        //   and one `state = -1` (destroyed/expired -> must be filtered out).
        // - Owner notes with `character_id == -79` out of order (`notification_id: 20` then `10`) plus one for another char.
        // - Corrupt JSON in `server_runtime_json` (must fall back cleanly to default without panicking).
        let notif_20 = super::super::cache_notifications::NotificationRecord {
            notification_id: 20,
            type_id: 5,
            group_id: 1,
            sender_id: 100,
            receiver_id: 200,
            processed: false,
            created: 2_000,
            data: String::new(),
            deleted: false,
            interacted: false,
        };
        let notif_10 = super::super::cache_notifications::NotificationRecord {
            notification_id: 10,
            type_id: 5,
            group_id: 1,
            sender_id: 100,
            receiver_id: 200,
            processed: true,
            created: 1_000,
            data: String::new(),
            deleted: false,
            interacted: false,
        };

        ctx.db.with_mut_snapshot(|snap| {
            snap.sp6_lifecycle.active_scanner_probes.insert(
                501,
                ActiveScannerProbe {
                    probe_id: 501,
                    probe_type_id: 30_013,
                    owner_char_id: 9_001,
                    ship_id: 200_125_728,
                    launcher_module_id: 10_002,
                    solar_system_id: 30_000_142,
                    position: [1.0, 2.0, 3.0],
                    destination: [4.0, 5.0, 6.0],
                    scan_range_m: 1_000_000.0,
                    range_step: 3,
                    base_sensor_strength: 40.0,
                    max_flight_time_s: 3600.0,
                    launched_at_unix: 1000,
                    state: 2,
                    warp_end_ft: Some(888_888),
                },
            );
            snap.sp6_lifecycle.active_scanner_probes.insert(
                502,
                ActiveScannerProbe {
                    probe_id: 502,
                    probe_type_id: 30_013,
                    owner_char_id: 9_001,
                    ship_id: 200_125_728,
                    launcher_module_id: 10_002,
                    solar_system_id: 30_000_142,
                    position: [0.0; 3],
                    destination: [0.0; 3],
                    scan_range_m: 500_000.0,
                    range_step: 1,
                    base_sensor_strength: 40.0,
                    max_flight_time_s: 3600.0,
                    launched_at_unix: 1000,
                    state: -1,
                    warp_end_ft: None,
                },
            );
            snap.owner_notes.insert(
                1,
                OwnerNoteRecord {
                    note_id: 1,
                    character_id: -79,
                    item_id: 0,
                    label: "n20".into(),
                    note: serde_json::to_string(&notif_20).unwrap(),
                    timestamp_unix: 2_000,
                },
            );
            snap.owner_notes.insert(
                2,
                OwnerNoteRecord {
                    note_id: 2,
                    character_id: -79,
                    item_id: 0,
                    label: "n10".into(),
                    note: serde_json::to_string(&notif_10).unwrap(),
                    timestamp_unix: 1_000,
                },
            );
            snap.owner_notes.insert(
                3,
                OwnerNoteRecord {
                    note_id: 3,
                    character_id: 9_001,
                    item_id: 0,
                    label: "player_note".into(),
                    note: serde_json::to_string(&notif_20).unwrap(),
                    timestamp_unix: 2_000,
                },
            );
            snap.cluster_and_runtime.server_runtime_json = Some("{corrupt_json".into());
        });

        ctx.hydrate_server_runtime_from_db();

        {
            let probes = ctx.active_scanner_probes.read().unwrap();
            assert_eq!(probes.len(), 1);
            assert_eq!(probes[&501].state, 0);
            assert_eq!(probes[&501].warp_end_ft, None);
            assert!(!probes.contains_key(&502));
        }

        let notifs = ctx.notification_records.read().unwrap();
        assert_eq!(notifs.len(), 2);
        assert_eq!(notifs[0].notification_id, 10);
        assert_eq!(notifs[1].notification_id, 20);
    }

    #[test]
    fn intern_ewar_kind_tactical_payload_tags_and_session_transition_helpers() {
        for k in [
            "warpScramblerMWD",
            "warpScrambler",
            "focusedWarpScrambler",
            "webify",
            "electronic",
            "ewRemoteSensorDamp",
            "ewGuidanceDisrupt",
            "ewTrackingDisrupt",
            "ewTargetPaint",
            "ewEnergyNeut",
            "ewEnergyVampire",
        ] {
            assert_eq!(intern_ewar_kind(k), k);
        }
        assert_eq!(intern_ewar_kind("unknown_ewar"), "electronic");

        // PersistedTacticalModulesPayload::is_empty and tag/untag round-trip across db_key vs inst_key
        let mut st = TacticalModulesState::default();
        let empty_p = PersistedTacticalModulesPayload::from_state(&st, 200);
        assert!(empty_p.is_empty());

        st.system_sim_time_s.insert((100, 30_000_142), 99.0);
        st.system_sim_time_s.insert((200, 30_000_144), 88.0);
        let non_empty_p = PersistedTacticalModulesPayload::from_state(&st, 200);
        assert!(!non_empty_p.is_empty());

        let mut restored = TacticalModulesState::default();
        let legacy_durable = non_empty_p.apply_to_state(&mut restored, 300, 400);
        assert!(legacy_durable.is_empty());
        assert_eq!(restored.system_sim_time_s.get(&(300, 30_000_142)), Some(&99.0));
        assert_eq!(restored.system_sim_time_s.get(&(400, 30_000_144)), Some(&88.0));

        // session_transition helpers: clear_session_character_state, apply_session_from_character, build_session_state_keyval, spec builders
        let ctx = ServerContext::new();
        let char_id = 90_000_001_i64;
        let ch = ctx.db.with_snapshot(|s| s.characters[&char_id].clone());
        let mut sess = eve_machonet::SessionState::new(42);
        sess.user_id = Some(ch.user_id);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(ch.corporation_id);

        crate::router::session_transition::apply_session_from_character(
            &mut sess,
            &ch,
            Some(Some(777_888)),
            Some(&ctx.sde),
        );
        assert_eq!(sess.ship_id, Some(777_888));
        assert_eq!(sess.solar_system_id, Some(ch.solar_system_id));
        let kv_docked = crate::router::session_transition::build_session_state_keyval(&sess, char_id);
        assert_eq!(kv_docked.dict_get("solarsystemid"), Some(&eve_proto::PyRep::None));
        assert_eq!(kv_docked.dict_get("shipid"), Some(&eve_proto::PyRep::Integer(777_888)));

        sess.station_id = None;
        sess.structure_id = None;
        let kv_space = crate::router::session_transition::build_session_state_keyval(&sess, char_id);
        assert_eq!(
            kv_space.dict_get("solarsystemid"),
            Some(&eve_proto::PyRep::Integer(i64::from(ch.solar_system_id)))
        );

        crate::router::session_transition::clear_session_character_state(&mut sess);
        assert!(sess.station_id.is_none());
        assert!(sess.structure_id.is_none());
        assert!(sess.solar_system_id.is_none());
        assert!(sess.constellation_id.is_none());
        assert!(sess.region_id.is_none());
        assert!(sess.ship_id.is_none());

        let spec = crate::router::session_transition::SessionTransitionSpec::for_kind(
            char_id,
            crate::router::session_transition::TransitionKind::Undock {
                solar_system_id: 30_000_142,
            },
            "test",
        )
        .with_apply_db(false)
        .with_move_ship_item(false)
        .with_validate_destination(false)
        .with_emit_on_session_changed(false)
        .with_sync_local_chat(false)
        .with_invalidate_bounds(false)
        .with_cleanup_departing_space(true)
        .with_arrive_ball_pos(glam::DVec3::new(1.0, 2.0, 3.0))
        .with_accept_ballpark_dock(60_003_760, glam::DVec3::ZERO)
        .with_preserve_session_ship(true);
        assert!(!spec.apply_db);
        assert!(!spec.move_ship_item);
        assert!(!spec.validate_destination);
        assert!(!spec.emit_on_session_changed);
        assert!(!spec.sync_local_chat);
        assert!(!spec.invalidate_bounds);
        assert!(spec.cleanup_departing_space);
        assert!(spec.spawn_destination_ball);
        assert_eq!(spec.arrive_ball_pos, Some(glam::DVec3::new(1.0, 2.0, 3.0)));
        assert_eq!(spec.accept_ballpark_dock, Some((60_003_760, glam::DVec3::ZERO)));
        assert!(spec.preserve_session_ship);
    }

    #[tokio::test]
    async fn system_checkpoint_persists_checkpointed_kinds_by_system_and_drops_crash_lost_kinds() {
        let ctx = test_ctx();
        let h_jita = ctx.systems.get_or_spawn(30_000_142);
        ctx.scanned_signatures
            .write()
            .unwrap()
            .insert((90_000_001, 30_000_142, 777));
        ctx.seed_ewar_source_for_test(Some(30_000_142), 500_001, 600_001);

        ctx.persist_server_runtime_to_db();

        // Checkpointed signature is persisted under Jita (30000142).
        let sig_rows = ctx
            .db
            .with_sqlite_conn(|conn| {
                load_all_checkpoints_for_kind::<(i64, u32, i64)>(conn, KIND_SCAN_SIGNATURES)
            })
            .expect("load signature checkpoints");
        assert_eq!(sig_rows.len(), 1);
        assert_eq!(sig_rows[0].0, 30_000_142);

        // Rehydrating restores checkpointed kinds and leaves crash-lost maps cleared.
        ctx.scanned_signatures.write().unwrap().clear();
        ctx.hydrate_server_runtime_from_db();
        assert!(ctx
            .scanned_signatures
            .read()
            .unwrap()
            .contains(&(90_000_001, 30_000_142, 777)));
        assert!(h_jita.ewar_state_snapshot().projected_ewar_sources.is_empty());
        assert!(crate::system_actor::SystemView::default().active_warps.is_empty());
    }

    #[tokio::test]
    async fn clear_crash_lost_runtime_maps_drops_tactical_lock_before_clearing_external_active_modules() {
        let ctx = test_ctx();
        let sys_id = 30_000_142_u32;
        let sys_h = ctx.systems.get_or_spawn(sys_id);

        // 1. Direct `clear_crash_lost_runtime_maps` must drop `tactical_state` before
        //    acquiring `SystemActorState` (`RANK_ACTOR_STATE < RANK_LEAF`), clearing both
        //    `sov_runtime` entosis maps and actor-owned external active modules + cached views.
        ctx.systems.register_external_active_module(sys_id, 700_001, 800_001);
        assert!(ctx.systems.is_module_active_sync(sys_id, 700_001, 800_001));
        assert!(sys_h.active_module_ships.read().unwrap().contains(&700_001));
        assert!(sys_h.view().has_active_module(700_001));

        ctx.db.with_mut_snapshot(|snap| {
            snap.sov_runtime.entosis_active_links.insert(
                700_001,
                eve_db::ActiveEntosisLinkRecord {
                    ship_id: 700_001,
                    char_id: 90_000_001,
                    side_id: 500_001,
                    module_id: 800_001,
                    target_id: 80,
                    cycle_index: 1,
                    cycle_duration_s: 120.0,
                    started_unix: 10,
                    expires_unix: 130,
                    is_capital: false,
                },
            );
            snap.sov_runtime.entosis_target_cycles.insert((80, 700_001), 2);
        });
        clear_crash_lost_runtime_maps(&ctx);
        ctx.db.with_snapshot(|snap| {
            assert!(snap.sov_runtime.entosis_active_links.is_empty());
            assert!(snap.sov_runtime.entosis_target_cycles.is_empty());
        });
        assert!(!ctx.systems.is_module_active_sync(sys_id, 700_001, 800_001));
        assert!(!sys_h.active_module_ships.read().unwrap().contains(&700_001));
        assert!(!sys_h.view().has_active_module(700_001));

        // 2. Full `hydrate_server_runtime_from_db()` path clears seeded `sov_runtime` entosis maps
        //    and actor external active modules cleanly.
        ctx.db.with_mut_snapshot(|snap| {
            snap.sov_runtime.entosis_active_links.insert(
                700_001,
                eve_db::ActiveEntosisLinkRecord {
                    ship_id: 700_001,
                    char_id: 90_000_001,
                    side_id: 500_001,
                    module_id: 800_002,
                    target_id: 81,
                    cycle_index: 1,
                    cycle_duration_s: 120.0,
                    started_unix: 10,
                    expires_unix: 130,
                    is_capital: false,
                },
            );
            snap.sov_runtime.entosis_target_cycles.insert((81, 700_001), 3);
        });
        ctx.systems.register_external_active_module(sys_id, 700_001, 800_002);
        assert!(sys_h.view().has_active_module(700_001));
        ctx.hydrate_server_runtime_from_db();
        ctx.db.with_snapshot(|snap| {
            assert!(snap.sov_runtime.entosis_active_links.is_empty());
            assert!(snap.sov_runtime.entosis_target_cycles.is_empty());
        });
        assert!(!ctx.systems.is_module_active_sync(sys_id, 700_001, 800_002));
        assert!(!sys_h.active_module_ships.read().unwrap().contains(&700_001));
        assert!(!sys_h.view().has_active_module(700_001));
    }

    #[test]
    fn legacy_cvb_checkpoint_row_and_payload_field_are_skipped_without_error() {
        // 1. Old `cvb|...` checkpoint rows (both solo and alongside valid durable tactical rows)
        //    must load cleanly without error, while non-`cvb|` corrupt rows still fail loudly.
        let ctx_solo = test_ctx();
        ctx_solo
            .db
            .with_sqlite_conn(|conn| {
                eve_db::sqlite_store::system_checkpoint::upsert_system_checkpoint(
                    conn,
                    CLUSTER_SYSTEM_ID,
                    KIND_TACTICAL_DURABLE,
                    "cvb|0|120",
                    &serde_json::json!({ "CloneVatBayActivatedShip": [0, 120] }),
                    1_800_000_000,
                )
            })
            .expect("seed solo legacy cvb checkpoint");
        assert!(matches!(
            load_checkpoints_into_ctx(&ctx_solo, true),
            CheckpointLoadOutcome::Loaded
        ));
        ctx_solo.hydrate_server_runtime_from_db();

        let ctx = test_ctx();
        ctx.db
            .with_sqlite_conn(|conn| -> Result<(), String> {
                eve_db::sqlite_store::system_checkpoint::upsert_system_checkpoint(
                    conn,
                    CLUSTER_SYSTEM_ID,
                    KIND_TACTICAL_DURABLE,
                    "cvb|0|120",
                    &serde_json::json!({ "CloneVatBayActivatedShip": [0, 120] }),
                    1_800_000_000,
                )?;
                eve_db::sqlite_store::system_checkpoint::upsert_system_checkpoint(
                    conn,
                    CLUSTER_SYSTEM_ID,
                    KIND_TACTICAL_DURABLE,
                    "cvb|1|121",
                    &serde_json::json!({ "CloneVatBayActivatedShip": [1, 121] }),
                    1_800_000_000,
                )?;
                eve_db::sqlite_store::system_checkpoint::upsert_system_checkpoint(
                    conn,
                    CLUSTER_SYSTEM_ID,
                    KIND_TACTICAL_DURABLE,
                    "lcl|0|500",
                    &DurableTacticalEntry::LanceCasterLockUntil(0, 500, 1_800_000_999),
                    1_800_000_000,
                )
            })
            .expect("seed mixed legacy cvb + valid durable tactical checkpoints");
        assert!(matches!(
            load_checkpoints_into_ctx(&ctx, true),
            CheckpointLoadOutcome::Loaded
        ));
        ctx.db.with_snapshot(|snap| {
            assert_eq!(
                snap.combat_crimewatch_state
                    .lance_caster_lock_until
                    .get(&500),
                Some(&1_800_000_999)
            );
        });

        // Re-persisting drops the obsolete `cvb|...` and migrated `lcl|...` rows while keeping DB-owned state.
        ctx.persist_server_runtime_to_db();
        let remaining_keys: Vec<String> = ctx
            .db
            .with_sqlite_conn(|conn| {
                load_all_checkpoints_for_kind::<serde_json::Value>(conn, KIND_TACTICAL_DURABLE)
            })
            .expect("reload tactical durable keys")
            .into_iter()
            .map(|(_, k, _)| k)
            .collect();
        assert!(
            remaining_keys.is_empty(),
            "expected all legacy KIND_TACTICAL_DURABLE rows to be pruned, got {remaining_keys:?}"
        );
        ctx.db.with_snapshot(|snap| {
            assert_eq!(
                snap.combat_crimewatch_state
                    .lance_caster_lock_until
                    .get(&500),
                Some(&1_800_000_999)
            );
        });

        // Genuinely corrupt non-`cvb|` row still produces CheckpointLoadOutcome::Error.
        ctx.db
            .with_sqlite_conn(|conn| {
                eve_db::sqlite_store::system_checkpoint::upsert_system_checkpoint(
                    conn,
                    CLUSTER_SYSTEM_ID,
                    KIND_TACTICAL_DURABLE,
                    "lcl|0|999",
                    &serde_json::json!({ "UnexpectedVariant": [0, 999] }),
                    1_800_000_000,
                )
            })
            .expect("seed corrupt non-cvb checkpoint");
        assert!(matches!(
            load_checkpoints_into_ctx(&ctx, true),
            CheckpointLoadOutcome::Error(_)
        ));

        // 2. Legacy PersistedServerRuntimeState / PersistedTacticalModulesPayload JSON carrying
        //    the removed field deserializes cleanly and hydrates/migrates remaining fields.
        let ctx_legacy = test_ctx();
        let legacy_field = ["clone_vat_bay", "activated_ships"].join("_");
        let legacy_payload = serde_json::json!({
            "map_landmarks": [[1, "LegacyMark", 1.0, 2.0, 3.0]],
            "tactical_state": {
                legacy_field: [[0, 120], [1, 121]],
                "entosis_active_links": [[[0, 700_001], {
                    "ship_id": 700_001,
                    "char_id": 90_000_001,
                    "side_id": 500_001,
                    "module_id": 800_001,
                    "target_id": 80,
                    "cycle_index": 1,
                    "cycle_duration_s": 120.0,
                    "started_unix": 10,
                    "expires_unix": 130,
                    "is_capital": false
                }]],
                "entosis_target_cycles": [[[0, 80, 700_001], 2]],
                "lance_caster_lock_until": [[[0, 500], 1_800_000_777]]
            }
        });
        ctx_legacy.db.with_mut_snapshot(|snap| {
            snap.cluster_and_runtime.server_runtime_json = Some(legacy_payload.to_string());
        });
        ctx_legacy.hydrate_server_runtime_from_db();
        ctx_legacy.db.with_snapshot(|snap| {
            assert_eq!(
                snap.combat_crimewatch_state
                    .lance_caster_lock_until
                    .get(&500),
                Some(&1_800_000_777)
            );
            assert!(snap.sov_runtime.entosis_active_links.is_empty());
            assert!(snap.sov_runtime.entosis_target_cycles.is_empty());
        });
        assert_eq!(ctx_legacy.map_landmarks.read().unwrap().len(), 1);
    }

    #[test]
    fn legacy_tsb_and_lance_checkpoints_and_payload_migrate_with_max_and_clean_hydrate_is_zero_write() {
        // 1. Legacy `tsb|...`, `lcl|...`, and `lvd|...` checkpoint rows migrate via `max(existing, v)`
        //    into `combat_crimewatch_state`, strictly outside any `tactical_state` lock.
        let ctx = test_ctx();
        ctx.db.with_mut_snapshot(|snap| {
            let cw = snap.combat_crimewatch_state.as_inner_mut();
            cw.tsb_self_penalty_until.insert(110, 700);
            cw.tsb_self_penalty_until.insert(111, 200);
            cw.lance_caster_lock_until.insert(501, 1_800_001_000);
            cw.lance_victim_debuff_until.insert(601, 1_800_002_000);
        });
        ctx.db
            .with_sqlite_conn(|conn| -> Result<(), String> {
                eve_db::sqlite_store::system_checkpoint::upsert_system_checkpoint(
                    conn,
                    CLUSTER_SYSTEM_ID,
                    KIND_TACTICAL_DURABLE,
                    "tsb|0|110",
                    &DurableTacticalEntry::TsbSelfPenaltyUntil(0, 110, 400),
                    1_800_000_000,
                )?;
                eve_db::sqlite_store::system_checkpoint::upsert_system_checkpoint(
                    conn,
                    CLUSTER_SYSTEM_ID,
                    KIND_TACTICAL_DURABLE,
                    "tsb|1|111",
                    &DurableTacticalEntry::TsbSelfPenaltyUntil(1, 111, 300),
                    1_800_000_000,
                )?;
                eve_db::sqlite_store::system_checkpoint::upsert_system_checkpoint(
                    conn,
                    CLUSTER_SYSTEM_ID,
                    KIND_TACTICAL_DURABLE,
                    "tsb|0|112",
                    &DurableTacticalEntry::TsbSelfPenaltyUntil(0, 112, 500),
                    1_800_000_000,
                )?;
                eve_db::sqlite_store::system_checkpoint::upsert_system_checkpoint(
                    conn,
                    CLUSTER_SYSTEM_ID,
                    KIND_TACTICAL_DURABLE,
                    "tsb|1|112",
                    &DurableTacticalEntry::TsbSelfPenaltyUntil(1, 112, 600),
                    1_800_000_000,
                )?;
                eve_db::sqlite_store::system_checkpoint::upsert_system_checkpoint(
                    conn,
                    CLUSTER_SYSTEM_ID,
                    KIND_TACTICAL_DURABLE,
                    "lcl|0|500",
                    &DurableTacticalEntry::LanceCasterLockUntil(0, 500, 1_800_000_888),
                    1_800_000_000,
                )?;
                eve_db::sqlite_store::system_checkpoint::upsert_system_checkpoint(
                    conn,
                    CLUSTER_SYSTEM_ID,
                    KIND_TACTICAL_DURABLE,
                    "lcl|1|500",
                    &DurableTacticalEntry::LanceCasterLockUntil(1, 500, 1_800_000_999),
                    1_800_000_000,
                )?;
                eve_db::sqlite_store::system_checkpoint::upsert_system_checkpoint(
                    conn,
                    CLUSTER_SYSTEM_ID,
                    KIND_TACTICAL_DURABLE,
                    "lcl|0|501",
                    &DurableTacticalEntry::LanceCasterLockUntil(0, 501, 1_800_000_500),
                    1_800_000_000,
                )?;
                eve_db::sqlite_store::system_checkpoint::upsert_system_checkpoint(
                    conn,
                    CLUSTER_SYSTEM_ID,
                    KIND_TACTICAL_DURABLE,
                    "lvd|0|600",
                    &DurableTacticalEntry::LanceVictimDebuffUntil(0, 600, 1_800_001_555),
                    1_800_000_000,
                )?;
                eve_db::sqlite_store::system_checkpoint::upsert_system_checkpoint(
                    conn,
                    CLUSTER_SYSTEM_ID,
                    KIND_TACTICAL_DURABLE,
                    "lvd|1|600",
                    &DurableTacticalEntry::LanceVictimDebuffUntil(1, 600, 1_800_001_444),
                    1_800_000_000,
                )?;
                eve_db::sqlite_store::system_checkpoint::upsert_system_checkpoint(
                    conn,
                    CLUSTER_SYSTEM_ID,
                    KIND_TACTICAL_DURABLE,
                    "lvd|0|601",
                    &DurableTacticalEntry::LanceVictimDebuffUntil(0, 601, 1_800_001_000),
                    1_800_000_000,
                )
            })
            .expect("seed legacy tactical durable checkpoints");

        ctx.hydrate_server_runtime_from_db();
        ctx.db.with_snapshot(|snap| {
            let cw = &snap.combat_crimewatch_state;
            assert_eq!(cw.tsb_self_penalty_until.get(&110), Some(&700), "existing higher timestamp must win");
            assert_eq!(cw.tsb_self_penalty_until.get(&111), Some(&300), "legacy higher timestamp must upgrade existing");
            assert_eq!(cw.tsb_self_penalty_until.get(&112), Some(&600), "duplicate legacy tags must take max");
            assert_eq!(cw.lance_caster_lock_until.get(&500), Some(&1_800_000_999));
            assert_eq!(cw.lance_caster_lock_until.get(&501), Some(&1_800_001_000));
            assert_eq!(cw.lance_victim_debuff_until.get(&600), Some(&1_800_001_555));
            assert_eq!(cw.lance_victim_debuff_until.get(&601), Some(&1_800_002_000));
        });

        // Re-hydrating when already dominated performs ZERO DB writes.
        let tx_before_noop = ctx.db.tx_commit_count();
        let epoch_before_noop = ctx.db.state_epoch();
        ctx.hydrate_server_runtime_from_db();
        assert_eq!(ctx.db.tx_commit_count(), tx_before_noop);
        assert_eq!(ctx.db.state_epoch(), epoch_before_noop);

        // Re-persisting drops all legacy `tsb|...`, `lcl|...`, and `lvd|...` checkpoint rows while preserving DB-owned state.
        ctx.persist_server_runtime_to_db();
        let remaining_keys: Vec<String> = ctx
            .db
            .with_sqlite_conn(|conn| {
                load_all_checkpoints_for_kind::<serde_json::Value>(conn, KIND_TACTICAL_DURABLE)
            })
            .expect("reload tactical durable keys")
            .into_iter()
            .map(|(_, k, _)| k)
            .collect();
        assert!(remaining_keys.is_empty());
        ctx.db.with_snapshot(|snap| {
            let cw = &snap.combat_crimewatch_state;
            assert_eq!(cw.tsb_self_penalty_until.get(&112), Some(&600));
            assert_eq!(cw.lance_caster_lock_until.get(&500), Some(&1_800_000_999));
            assert_eq!(cw.lance_victim_debuff_until.get(&600), Some(&1_800_001_555));
        });

        // Clean hydrate with pruned checkpoints performs ZERO DB writes.
        let tx_clean = ctx.db.tx_commit_count();
        let epoch_clean = ctx.db.state_epoch();
        ctx.hydrate_server_runtime_from_db();
        assert_eq!(ctx.db.tx_commit_count(), tx_clean);
        assert_eq!(ctx.db.state_epoch(), epoch_clean);

        // 2. Clean hydrate on an empty repository also performs ZERO DB writes.
        let ctx_empty = test_ctx();
        let tx_empty = ctx_empty.db.tx_commit_count();
        let epoch_empty = ctx_empty.db.state_epoch();
        ctx_empty.hydrate_server_runtime_from_db();
        assert_eq!(ctx_empty.db.tx_commit_count(), tx_empty);
        assert_eq!(ctx_empty.db.state_epoch(), epoch_empty);

        // 3. Legacy `PersistedTacticalModulesPayload` JSON carrying `tsb_self_penalty_until`,
        //    `lance_caster_lock_until`, and `lance_victim_debuff_until` is non-empty and
        //    migrates via `max(existing, v)` without deadlock.
        let ctx_json = test_ctx();
        ctx_json.db.with_mut_snapshot(|snap| {
            let cw = snap.combat_crimewatch_state.as_inner_mut();
            cw.tsb_self_penalty_until.insert(210, 750);
            cw.tsb_self_penalty_until.insert(211, 150);
            cw.lance_caster_lock_until.insert(220, 850);
            cw.lance_victim_debuff_until.insert(230, 950);
        });
        let legacy_json = serde_json::json!({
            "tactical_state": {
                "tsb_self_penalty_until": [[[0, 210], 400], [[1, 211], 650], [[0, 212], 500]],
                "lance_caster_lock_until": [[[0, 220], 400], [[1, 221], 900], [[0, 221], 800]],
                "lance_victim_debuff_until": [[[0, 230], 500], [[1, 231], 910], [[0, 231], 920]]
            }
        });
        let parsed_legacy: PersistedServerRuntimeState =
            serde_json::from_value(legacy_json.clone()).expect("parse legacy payload");
        assert!(!parsed_legacy.tactical_state.is_empty());

        ctx_json.db.with_mut_snapshot(|snap| {
            snap.cluster_and_runtime.server_runtime_json = Some(legacy_json.to_string());
        });
        ctx_json.hydrate_server_runtime_from_db();
        ctx_json.db.with_snapshot(|snap| {
            let cw = &snap.combat_crimewatch_state;
            assert_eq!(cw.tsb_self_penalty_until.get(&210), Some(&750));
            assert_eq!(cw.tsb_self_penalty_until.get(&211), Some(&650));
            assert_eq!(cw.tsb_self_penalty_until.get(&212), Some(&500));
            assert_eq!(cw.lance_caster_lock_until.get(&220), Some(&850));
            assert_eq!(cw.lance_caster_lock_until.get(&221), Some(&900));
            assert_eq!(cw.lance_victim_debuff_until.get(&230), Some(&950));
            assert_eq!(cw.lance_victim_debuff_until.get(&231), Some(&920));
            assert!(snap.cluster_and_runtime.server_runtime_json.is_none());
        });
        let json_keys: Vec<String> = ctx_json
            .db
            .with_sqlite_conn(|conn| {
                load_all_checkpoints_for_kind::<serde_json::Value>(conn, KIND_TACTICAL_DURABLE)
            })
            .expect("check no tactical durable checkpoints emitted")
            .into_iter()
            .map(|(_, k, _)| k)
            .collect();
        assert!(json_keys.is_empty());

        // 4. Write-lock guard: calling legacy migration / apply_to_ctx while holding
        //    `with_mut_snapshot` must skip DB writes without deadlocking.
        let ctx_locked = test_ctx();
        let legacy_locked: PersistedServerRuntimeState = serde_json::from_value(serde_json::json!({
            "tactical_state": {
                "tsb_self_penalty_until": [[[0, 333], 999]],
                "lance_caster_lock_until": [[[0, 334], 999]],
                "lance_victim_debuff_until": [[[0, 335], 999]]
            }
        }))
        .expect("parse legacy locked payload");
        ctx_locked.db.with_mut_snapshot(|snap| {
            let can_write = !ctx_locked.db.is_write_locked_by_current_thread();
            assert!(!can_write);
            merge_legacy_tactical_durable_into_db(
                &ctx_locked.db,
                LegacyTacticalDurableEntries {
                    lance_caster_lock_until: HashMap::from([(334, 999)]),
                    lance_victim_debuff_until: HashMap::from([(335, 999)]),
                    tsb_self_penalty_until: HashMap::from([(333, 999)]),
                },
                can_write,
            );
            legacy_locked.apply_to_ctx(&ctx_locked, can_write);
            assert!(!snap
                .combat_crimewatch_state
                .tsb_self_penalty_until
                .contains_key(&333));
            assert!(!snap
                .combat_crimewatch_state
                .lance_caster_lock_until
                .contains_key(&334));
            assert!(!snap
                .combat_crimewatch_state
                .lance_victim_debuff_until
                .contains_key(&335));
        });
    }
}
