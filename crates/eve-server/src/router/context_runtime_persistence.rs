//! Durable persistence and rollback rehydration for `ServerContext` runtime state (`#592`).
//!
//! Keeps ONLY transient network/handle/derived caches (`session_closers`, `notify_tx`,
//! `pending_notifications`, `bound_objects`, `object_cache*`, `ship_fittings`) in RAM while
//! persisting all gameplay-affecting runtime state inside `WorldStateSnapshot::service_state_blobs`.

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
use crate::services::mining_cycle::ActiveMiningCycle;
use crate::services::space_destiny::SessionChangeTimerState;
use crate::services::space_mining_harvest::PhasedAsteroidFieldState;
use crate::services::tactical_module_mechanics::{ActiveInterdictionProbe, LauncherFireStamp};
use crate::services::tactical_modules::{
    db_ptr_key, ActiveEntosisLinkState, ProjectedRemoteAssistance, TacticalModulesState,
};
use crate::services::tactical_modules_probes::{ActiveWubbleDebuff, PendingWubbleProbe};
use eve_db::FleetRole;
use eve_dogma::ProjectedEwarEffect;

pub(crate) const SERVER_RUNTIME_STATE_BLOB_KEY: &str = "server_runtime_state";

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
    entosis_active_links: Vec<((u8, i64), ActiveEntosisLinkState)>,
    entosis_target_cycles: Vec<((u8, i64, i64), u32)>,
    pending_wubble_probes: Vec<(u8, PendingWubbleProbe)>,
    active_wubble_debuffs: Vec<((u8, i64), ActiveWubbleDebuff)>,
    tsb_self_penalty_until: Vec<((u8, i64), u64)>,
    clone_vat_bay_activated_ships: Vec<(u8, i64)>,
    active_interdiction_probes: Vec<((u8, i64), ActiveInterdictionProbe)>,
    launcher_last_fire: Vec<((u8, i64), LauncherFireStamp)>,
    system_sim_time_s: Vec<((u8, u32), f64)>,
}

impl PersistedTacticalModulesPayload {
    #[cfg(test)]
    fn from_state(st: &TacticalModulesState, cur_inst_key: usize) -> Self {
        Self::from_state_for_keys(st, None, cur_inst_key)
    }

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
            lance_caster_lock_until: st
                .lance_caster_lock_until
                .iter()
                .filter(|(&(k, _), _)| keep_key(k))
                .map(|(&(k, sid), &v)| ((tag_of(k), sid), v))
                .collect(),
            lance_victim_debuff_until: st
                .lance_victim_debuff_until
                .iter()
                .filter(|(&(k, _), _)| keep_key(k))
                .map(|(&(k, sid), &v)| ((tag_of(k), sid), v))
                .collect(),
            entosis_active_links: st
                .entosis_active_links
                .iter()
                .filter(|(&(k, _), _)| keep_key(k))
                .map(|(&(k, sid), v)| ((tag_of(k), sid), v.clone()))
                .collect(),
            entosis_target_cycles: st
                .entosis_target_cycles
                .iter()
                .filter(|(&(k, _, _), _)| keep_key(k))
                .map(|(&(k, sid, tid), &v)| ((tag_of(k), sid, tid), v))
                .collect(),
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
            tsb_self_penalty_until: st
                .tsb_self_penalty_until
                .iter()
                .filter(|(&(k, _), _)| keep_key(k))
                .map(|(&(k, sid), &v)| ((tag_of(k), sid), v))
                .collect(),
            clone_vat_bay_activated_ships: st
                .clone_vat_bay_activated_ships
                .iter()
                .filter(|&&(k, _)| keep_key(k))
                .map(|&(k, sid)| (tag_of(k), sid))
                .collect(),
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
        payload.lance_caster_lock_until.sort_by_key(|(k, _)| *k);
        payload.lance_victim_debuff_until.sort_by_key(|(k, _)| *k);
        payload.entosis_active_links.sort_by_key(|(k, _)| *k);
        payload.entosis_target_cycles.sort_by_key(|(k, _)| *k);
        payload.active_wubble_debuffs.sort_by_key(|(k, _)| *k);
        payload.tsb_self_penalty_until.sort_by_key(|(k, _)| *k);
        payload.clone_vat_bay_activated_ships.sort_unstable();
        payload.active_interdiction_probes.sort_by_key(|(k, _)| *k);
        payload.launcher_last_fire.sort_by_key(|(k, _)| *k);
        payload.system_sim_time_s.sort_by_key(|(k, _)| *k);
        payload
    }

    fn is_empty(&self) -> bool {
        self.remote_assistance.is_empty()
            && self.lance_caster_lock_until.is_empty()
            && self.lance_victim_debuff_until.is_empty()
            && self.entosis_active_links.is_empty()
            && self.entosis_target_cycles.is_empty()
            && self.pending_wubble_probes.is_empty()
            && self.active_wubble_debuffs.is_empty()
            && self.tsb_self_penalty_until.is_empty()
            && self.clone_vat_bay_activated_ships.is_empty()
            && self.active_interdiction_probes.is_empty()
            && self.launcher_last_fire.is_empty()
            && self.system_sim_time_s.is_empty()
    }

    fn apply_to_state(self, st: &mut TacticalModulesState, cur_db_key: usize, cur_inst_key: usize) {
        let untag = |tag: u8| -> usize { if tag == 1 { cur_inst_key } else { cur_db_key } };
        st.remote_assistance = self
            .remote_assistance
            .into_iter()
            .map(|((tag, sid), v)| ((untag(tag), sid), v))
            .collect();
        st.lance_caster_lock_until = self
            .lance_caster_lock_until
            .into_iter()
            .map(|((tag, sid), v)| ((untag(tag), sid), v))
            .collect();
        st.lance_victim_debuff_until = self
            .lance_victim_debuff_until
            .into_iter()
            .map(|((tag, sid), v)| ((untag(tag), sid), v))
            .collect();
        st.entosis_active_links = self
            .entosis_active_links
            .into_iter()
            .map(|((tag, sid), v)| ((untag(tag), sid), v))
            .collect();
        st.entosis_target_cycles = self
            .entosis_target_cycles
            .into_iter()
            .map(|((tag, sid, tid), v)| ((untag(tag), sid, tid), v))
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
        st.tsb_self_penalty_until = self
            .tsb_self_penalty_until
            .into_iter()
            .map(|((tag, sid), v)| ((untag(tag), sid), v))
            .collect();
        st.clone_vat_bay_activated_ships = self
            .clone_vat_bay_activated_ships
            .into_iter()
            .map(|(tag, sid)| (untag(tag), sid))
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
    }
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

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct PersistedSocialExtrasRuntimePayload {
    delayed_local_speakers: HashMap<String, HashSet<i64>>,
    active_probes: HashMap<i64, Vec<(i64, [f64; 3], f64, u64)>>,
    forced_session_corp_sync: HashMap<i64, (i64, Option<i64>, u64)>,
    forced_session_ship_sync: HashMap<i64, (i64, Option<i64>)>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct PersistedServerRuntimeState {
    // Active scanner probes are stored in `sp6_lifecycle.active_scanner_probes` (typed SQLite
    // table) rather than the 4096-byte `service_state_blobs` row so 8-probe launches never blow
    // the blob cap and cluster-restart downtime recovery (`state = 0`) is never clobbered.
    active_moon_probes: Vec<ActiveMoonSurveyProbe>,
    scanned_signatures: Vec<(i64, u32, i64)>,
    character_scan_progress: Vec<((i64, u32, String), f64)>,
    active_fighters: HashMap<i64, ActiveFighterSquadron>,
    fighter_cooldown_state: Vec<((i64, u32), u64)>,
    fighter_charge_state: Vec<((i64, u32), u32)>,
    fighter_tube_loads: Vec<((i64, usize), i64)>,
    module_reactivation_cooldowns: Vec<((i64, i64, u32), ModuleCooldownEntry)>,
    mining_cycles: HashMap<i64, ActiveMiningCycle>,
    phased_asteroid_fields: HashMap<u32, PhasedAsteroidFieldState>,
    session_change_timers: HashMap<i64, SessionChangeTimerState>,
    active_warps: HashMap<i64, i64>,
    location_change_invuln: HashMap<i64, u64>,
    weapon_cycle_state: HashMap<i64, (Option<u64>, u32, Option<i64>)>,
    projected_ewar: HashMap<i64, Vec<ProjectedEwarEffect>>,
    projected_ewar_sources: HashMap<i64, i64>,
    active_ewar_starts: Vec<((i64, i64), Vec<(i64, String)>)>,
    map_landmarks: Vec<(i64, String, f64, f64, f64)>,
    tactical_state: PersistedTacticalModulesPayload,
    fleet_extras_runtime: PersistedFleetExtrasPayload,
    social_extras_runtime: PersistedSocialExtrasRuntimePayload,
}

impl PersistedServerRuntimeState {
    fn from_ctx(ctx: &ServerContext) -> Self {
        let cur_db_key = db_ptr_key(&ctx.db);
        let cur_inst_key = ctx.instance_id as usize;
        let tactical_state = {
            let tac = ctx.tactical_state.read().unwrap_or_else(|e| e.into_inner());
            PersistedTacticalModulesPayload::from_state_for_keys(&tac, Some(cur_db_key), cur_inst_key)
        };
        let fleet_extras_runtime = {
            let ex = ctx.fleet_extras.lock().unwrap_or_else(|e| e.into_inner());
            let mut invite_details: Vec<_> =
                ex.invite_details.iter().map(|(&k, v)| (k, v.clone())).collect();
            invite_details.sort_by_key(|(k, _)| *k);
            let mut invite_timestamps: Vec<_> =
                ex.invite_timestamps.iter().map(|(&k, &v)| (k, v)).collect();
            invite_timestamps.sort_by_key(|(k, _)| *k);
            PersistedFleetExtrasPayload {
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
        };
        let social_extras_runtime = {
            let se = ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner());
            PersistedSocialExtrasRuntimePayload {
                delayed_local_speakers: se.delayed_local_speakers.clone(),
                active_probes: se.active_probes.clone(),
                forced_session_corp_sync: se.forced_session_corp_sync.clone(),
                forced_session_ship_sync: se.forced_session_ship_sync.clone(),
            }
        };

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
        let (
            active_fighters,
            mut fighter_cooldown_state,
            mut fighter_charge_state,
            mut fighter_tube_loads,
        ) = {
            let fg = ctx.fighters.read().unwrap_or_else(|e| e.into_inner());
            (
                fg.active_fighters.clone(),
                fg.cooldown_state
                    .iter()
                    .map(|(&k, &v)| (k, v))
                    .collect::<Vec<_>>(),
                fg.charge_state
                    .iter()
                    .map(|(&k, &v)| (k, v))
                    .collect::<Vec<_>>(),
                fg.tube_loads
                    .iter()
                    .map(|(&k, &v)| (k, v))
                    .collect::<Vec<_>>(),
            )
        };
        fighter_cooldown_state.sort_by_key(|(k, _)| *k);
        fighter_charge_state.sort_by_key(|(k, _)| *k);
        fighter_tube_loads.sort_by_key(|(k, _)| *k);
        let mut module_reactivation_cooldowns: Vec<_> = ctx
            .module_reactivation_cooldowns
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(&k, &v)| (k, v))
            .collect();
        module_reactivation_cooldowns.sort_by_key(|(k, _)| *k);
        let mut active_ewar_starts: Vec<_> = ctx
            .active_ewar_starts
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(&k, v)| (k, v.iter().map(|&(tid, s)| (tid, s.to_string())).collect()))
            .collect();
        active_ewar_starts.sort_by_key(|(k, _)| *k);

        let active_moon_probes = ctx
            .active_moon_probes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mining_cycles = ctx
            .mining_cycles
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let phased_asteroid_fields = ctx
            .phased_asteroid_fields
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let session_change_timers = ctx
            .session_change_timers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let active_warps = ctx
            .active_warps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let location_change_invuln = ctx
            .location_change_invuln
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let weapon_cycle_state = ctx
            .weapon_cycle_state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let projected_ewar = ctx
            .projected_ewar
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let projected_ewar_sources = ctx
            .projected_ewar_sources
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
            active_fighters,
            fighter_cooldown_state,
            fighter_charge_state,
            fighter_tube_loads,
            module_reactivation_cooldowns,
            mining_cycles,
            phased_asteroid_fields,
            session_change_timers,
            active_warps,
            location_change_invuln,
            weapon_cycle_state,
            projected_ewar,
            projected_ewar_sources,
            active_ewar_starts,
            map_landmarks,
            tactical_state,
            fleet_extras_runtime,
            social_extras_runtime,
        }
    }

    fn is_empty(&self) -> bool {
        self.active_moon_probes.is_empty()
            && self.scanned_signatures.is_empty()
            && self.character_scan_progress.is_empty()
            && self.active_fighters.is_empty()
            && self.fighter_cooldown_state.is_empty()
            && self.fighter_charge_state.is_empty()
            && self.fighter_tube_loads.is_empty()
            && self.module_reactivation_cooldowns.is_empty()
            && self.mining_cycles.is_empty()
            && self.phased_asteroid_fields.is_empty()
            && self.session_change_timers.is_empty()
            && self.active_warps.is_empty()
            && self.location_change_invuln.is_empty()
            && self.weapon_cycle_state.is_empty()
            && self.projected_ewar.is_empty()
            && self.projected_ewar_sources.is_empty()
            && self.active_ewar_starts.is_empty()
            && self.map_landmarks.is_empty()
            && self.tactical_state.is_empty()
            && self.fleet_extras_runtime.fleet_names.is_empty()
            && self.fleet_extras_runtime.max_sizes.is_empty()
            && self.fleet_extras_runtime.auto_join_squads.is_empty()
            && self.fleet_extras_runtime.invite_details.is_empty()
            && self.fleet_extras_runtime.invite_timestamps.is_empty()
            && self.fleet_extras_runtime.disconnected_members.is_empty()
            && self.fleet_extras_runtime.adverts.is_empty()
            && self.fleet_extras_runtime.target_tags.is_empty()
            && self.fleet_extras_runtime.wing_squad_hierarchy.is_empty()
            && self.fleet_extras_runtime.saved_setups.is_empty()
            && self.fleet_extras_runtime.stored_formations.is_empty()
            && self.fleet_extras_runtime.broadcast_filters.is_empty()
            && self.fleet_extras_runtime.last_broadcast.is_empty()
            && self.fleet_extras_runtime.broadcast_history.is_empty()
            && self.social_extras_runtime.delayed_local_speakers.is_empty()
            && self.social_extras_runtime.active_probes.is_empty()
            && self.social_extras_runtime.forced_session_corp_sync.is_empty()
            && self.social_extras_runtime.forced_session_ship_sync.is_empty()
    }

    fn apply_to_ctx(self, ctx: &ServerContext) {
        *ctx.active_moon_probes
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.active_moon_probes;
        *ctx.scanned_signatures
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.scanned_signatures.into_iter().collect();
        *ctx.character_scan_progress
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.character_scan_progress.into_iter().collect();
        {
            let mut fg = ctx.fighters.write().unwrap_or_else(|e| e.into_inner());
            fg.active_fighters = self.active_fighters;
            fg.cooldown_state = self.fighter_cooldown_state.into_iter().collect();
            fg.charge_state = self.fighter_charge_state.into_iter().collect();
            fg.tube_loads = self.fighter_tube_loads.into_iter().collect();
        }
        *ctx.module_reactivation_cooldowns
            .write()
            .unwrap_or_else(|e| e.into_inner()) =
            self.module_reactivation_cooldowns.into_iter().collect();
        *ctx.mining_cycles
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.mining_cycles;
        *ctx.phased_asteroid_fields
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.phased_asteroid_fields;
        *ctx.session_change_timers
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = self.session_change_timers;
        *ctx.active_warps
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = self.active_warps;
        *ctx.location_change_invuln
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = self.location_change_invuln;
        *ctx.weapon_cycle_state
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.weapon_cycle_state;
        *ctx.projected_ewar
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.projected_ewar;
        *ctx.projected_ewar_sources
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.projected_ewar_sources;
        *ctx.active_ewar_starts
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self
            .active_ewar_starts
            .into_iter()
            .map(|(k, v)| {
                (
                    k,
                    v.into_iter()
                        .map(|(tid, s)| (tid, intern_ewar_kind(&s)))
                        .collect(),
                )
            })
            .collect();
        *ctx.map_landmarks
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.map_landmarks;

        let cur_db_key = db_ptr_key(&ctx.db);
        let cur_inst_key = ctx.instance_id as usize;
        {
            let mut tac = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
            self.tactical_state
                .apply_to_state(&mut tac, cur_db_key, cur_inst_key);
        }

        {
            let mut ex = ctx.fleet_extras.lock().unwrap_or_else(|e| e.into_inner());
            ex.fleet_names = self.fleet_extras_runtime.fleet_names;
            ex.max_sizes = self.fleet_extras_runtime.max_sizes;
            ex.auto_join_squads = self.fleet_extras_runtime.auto_join_squads;
            ex.invite_details = self.fleet_extras_runtime.invite_details.into_iter().collect();
            ex.invite_timestamps = self
                .fleet_extras_runtime
                .invite_timestamps
                .into_iter()
                .collect();
            ex.disconnected_members = self.fleet_extras_runtime.disconnected_members;
            ex.adverts = self.fleet_extras_runtime.adverts;
            ex.target_tags = self.fleet_extras_runtime.target_tags;
            ex.wing_squad_hierarchy = self.fleet_extras_runtime.wing_squad_hierarchy;
            ex.saved_setups = self.fleet_extras_runtime.saved_setups;
            ex.stored_formations = self.fleet_extras_runtime.stored_formations;
            ex.broadcast_filters = self.fleet_extras_runtime.broadcast_filters;
            ex.last_broadcast = self.fleet_extras_runtime.last_broadcast;
            ex.broadcast_history = self.fleet_extras_runtime.broadcast_history;
        }
        {
            let mut se = ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner());
            se.delayed_local_speakers = self.social_extras_runtime.delayed_local_speakers;
            se.active_probes = self.social_extras_runtime.active_probes;
            se.forced_session_corp_sync = self.social_extras_runtime.forced_session_corp_sync;
            se.forced_session_ship_sync = self.social_extras_runtime.forced_session_ship_sync;
        }
    }
}

impl ServerContext {
    /// Hydrates all gameplay-affecting `ServerContext` runtime maps from `WorldRepository` (`#592`).
    pub fn hydrate_server_runtime_from_db(&self) {
        if !self.db.is_write_locked_by_current_thread()
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
            .and_then(|s| serde_json::from_str::<PersistedServerRuntimeState>(&s).ok())
            .unwrap_or_default();
        state.apply_to_ctx(self);
    }

    /// Persists all gameplay-affecting `ServerContext` runtime maps into
    /// `WorldStateSnapshot::service_state_blobs` and `sp6_lifecycle.active_scanner_probes` (`#592`).
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
            crate::sp6_lifecycle::with_mut_sp6_state(&self.db, |st, _snap| {
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
        let payload = PersistedServerRuntimeState::from_ctx(self);
        if payload.is_empty() {
            let has_state = self.db.with_snapshot(|s| {
                s.cluster_and_runtime.server_runtime_json.is_some()
                    || s.sp6_lifecycle.server_runtime_json.is_some()
                    || s.service_state_blobs
                        .contains_key(SERVER_RUNTIME_STATE_BLOB_KEY)
            });
            if !has_state {
                return;
            }
            let _ = self.db.with_mut_snapshot(|snap| {
                snap.cluster_and_runtime.server_runtime_json = None;
                snap.sp6_lifecycle.server_runtime_json = None;
                snap.service_state_blobs
                    .remove(SERVER_RUNTIME_STATE_BLOB_KEY);
            });
            return;
        }
        if let Ok(json) = serde_json::to_string(&payload) {
            let already_matches = self.db.with_snapshot(|s| {
                s.cluster_and_runtime.server_runtime_json.as_deref() == Some(json.as_str())
                    && s.sp6_lifecycle.server_runtime_json.is_none()
                    && !s.service_state_blobs
                        .contains_key(SERVER_RUNTIME_STATE_BLOB_KEY)
            });
            if !already_matches {
                let _ = self.db.with_mut_snapshot(|snap| {
                    snap.cluster_and_runtime.server_runtime_json = Some(json);
                    snap.sp6_lifecycle.server_runtime_json = None;
                    snap.service_state_blobs
                        .remove(SERVER_RUNTIME_STATE_BLOB_KEY);
                });
            }
        }
    }

    /// Flushes all gameplay-affecting runtime state across `ServerContext`, `CombatRuntimeState`,
    /// social extras, and `QuasarGateway` into SQLite (`#592`).
    pub fn sync_runtime_to_db(&self) {
        self.systems.flush_all_condition_checkpoints_to_db();
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
        st.lance_caster_lock_until.insert((inst_k, 30), 100);
        st.lance_caster_lock_until.insert((db_k, 40), 200);
        st.lance_victim_debuff_until.insert((db_k, 50), 300);
        st.entosis_active_links.insert(
            (inst_k, 60),
            ActiveEntosisLinkState {
                ship_id: 60,
                char_id: 9_001,
                side_id: 500_001,
                module_id: 70,
                target_id: 80,
                cycle_index: 1,
                cycle_duration_s: 120.0,
                started_unix: 10,
                expires_unix: 130,
                is_capital: false,
            },
        );
        st.entosis_target_cycles.insert((db_k, 60, 80), 3);
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
        st.tsb_self_penalty_until.insert((inst_k, 110), 400);
        st.clone_vat_bay_activated_ships.insert((db_k, 120));
        st.clone_vat_bay_activated_ships.insert((inst_k, 121));
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
        assert_eq!(payload.lance_caster_lock_until[0].0, (0, 40));
        assert_eq!(payload.lance_caster_lock_until[1].0, (1, 30));

        // Remap into a fresh `new_db_k` and `new_inst_k`
        let new_db_k = 8_888_usize;
        let new_inst_k = 9_999_usize;
        let mut restored = TacticalModulesState::default();
        payload.apply_to_state(&mut restored, new_db_k, new_inst_k);
        assert!(restored.remote_assistance.contains_key(&(new_db_k, 10)));
        assert!(restored.remote_assistance.contains_key(&(new_inst_k, 20)));
        assert_eq!(restored.lance_caster_lock_until[&(new_db_k, 40)], 200);
        assert_eq!(restored.lance_caster_lock_until[&(new_inst_k, 30)], 100);
        assert_eq!(restored.lance_victim_debuff_until[&(new_db_k, 50)], 300);
        assert!(restored.entosis_active_links.contains_key(&(new_inst_k, 60)));
        assert_eq!(restored.entosis_target_cycles[&(new_db_k, 60, 80)], 3);
        assert_eq!(restored.pending_wubble_probes[0].0, new_inst_k);
        assert!(restored.active_wubble_debuffs.contains_key(&(new_db_k, 100)));
        assert_eq!(restored.tsb_self_penalty_until[&(new_inst_k, 110)], 400);
        assert!(restored.clone_vat_bay_activated_ships.contains(&(new_db_k, 120)));
        assert!(restored.clone_vat_bay_activated_ships.contains(&(new_inst_k, 121)));
        assert!(restored.active_interdiction_probes.contains_key(&(new_db_k, 130)));
        assert_eq!(restored.launcher_last_fire[&(new_inst_k, 140)].fired_unix, 500);
        assert_eq!(restored.system_sim_time_s[&(new_db_k, 30_000_142)], 42.5);
    }

    #[test]
    fn persisted_server_runtime_state_is_empty_and_write_lock_guard() {
        let ctx = test_ctx();
        assert!(PersistedServerRuntimeState::from_ctx(&ctx).is_empty());

        // 1. Re-entrancy guard: calling `persist_server_runtime_to_db` while holding `with_mut_snapshot`
        //    must return immediately without deadlocking or writing `server_runtime_json`.
        ctx.map_landmarks
            .write()
            .unwrap()
            .push((1, "Landmark".into(), 1.0, 2.0, 3.0));
        assert!(!PersistedServerRuntimeState::from_ctx(&ctx).is_empty());

        ctx.db.with_mut_snapshot(|snap| {
            ctx.persist_server_runtime_to_db();
            assert!(snap.cluster_and_runtime.server_runtime_json.is_none());
        });

        // 2. Outside the write lock, `persist_server_runtime_to_db` writes `server_runtime_json`
        //    and clears legacy `sp6_lifecycle.server_runtime_json` + `service_state_blobs`.
        ctx.db.with_mut_snapshot(|snap| {
            snap.sp6_lifecycle.server_runtime_json = Some("{\"legacy\":true}".into());
            snap.service_state_blobs
                .insert(SERVER_RUNTIME_STATE_BLOB_KEY.into(), "{\"legacy\":true}".into());
        });
        ctx.persist_server_runtime_to_db();
        ctx.db.with_snapshot(|snap| {
            assert!(snap.cluster_and_runtime.server_runtime_json.is_some());
            assert!(snap.sp6_lifecycle.server_runtime_json.is_none());
            assert!(!snap
                .service_state_blobs
                .contains_key(SERVER_RUNTIME_STATE_BLOB_KEY));
        });

        // 3. Clearing all runtime fields and persisting removes `server_runtime_json`.
        ctx.map_landmarks.write().unwrap().clear();
        assert!(PersistedServerRuntimeState::from_ctx(&ctx).is_empty());
        ctx.persist_server_runtime_to_db();
        ctx.db.with_snapshot(|snap| {
            assert!(snap.cluster_and_runtime.server_runtime_json.is_none());
        });
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

        st.lance_caster_lock_until.insert((100, 5001), 99);
        st.lance_caster_lock_until.insert((200, 5002), 88);
        let non_empty_p = PersistedTacticalModulesPayload::from_state(&st, 200);
        assert!(!non_empty_p.is_empty());

        let mut restored = TacticalModulesState::default();
        non_empty_p.apply_to_state(&mut restored, 300, 400);
        assert_eq!(restored.lance_caster_lock_until.get(&(300, 5001)), Some(&99));
        assert_eq!(restored.lance_caster_lock_until.get(&(400, 5002)), Some(&88));

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
}
