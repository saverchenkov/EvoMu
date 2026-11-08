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
use crate::services::combat_crimewatch::{flush_combat_runtime_to_db, sync_combat_runtime_from_db};
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
    fn from_state(st: &TacticalModulesState, cur_inst_key: usize) -> Self {
        let tag_of = |k: usize| -> u8 { u8::from(k == cur_inst_key) };
        let mut payload = Self {
            remote_assistance: st
                .remote_assistance
                .iter()
                .map(|(&(k, sid), v)| ((tag_of(k), sid), v.clone()))
                .collect(),
            lance_caster_lock_until: st
                .lance_caster_lock_until
                .iter()
                .map(|(&(k, sid), &v)| ((tag_of(k), sid), v))
                .collect(),
            lance_victim_debuff_until: st
                .lance_victim_debuff_until
                .iter()
                .map(|(&(k, sid), &v)| ((tag_of(k), sid), v))
                .collect(),
            entosis_active_links: st
                .entosis_active_links
                .iter()
                .map(|(&(k, sid), v)| ((tag_of(k), sid), v.clone()))
                .collect(),
            entosis_target_cycles: st
                .entosis_target_cycles
                .iter()
                .map(|(&(k, sid, tid), &v)| ((tag_of(k), sid, tid), v))
                .collect(),
            pending_wubble_probes: st
                .pending_wubble_probes
                .iter()
                .map(|(k, v)| (tag_of(*k), v.clone()))
                .collect(),
            active_wubble_debuffs: st
                .active_wubble_debuffs
                .iter()
                .map(|(&(k, sid), v)| ((tag_of(k), sid), v.clone()))
                .collect(),
            tsb_self_penalty_until: st
                .tsb_self_penalty_until
                .iter()
                .map(|(&(k, sid), &v)| ((tag_of(k), sid), v))
                .collect(),
            clone_vat_bay_activated_ships: st
                .clone_vat_bay_activated_ships
                .iter()
                .map(|&(k, sid)| (tag_of(k), sid))
                .collect(),
            active_interdiction_probes: st
                .active_interdiction_probes
                .iter()
                .map(|(&(k, pid), v)| ((tag_of(k), pid), v.clone()))
                .collect(),
            launcher_last_fire: st
                .launcher_last_fire
                .iter()
                .map(|(&(k, mid), &v)| ((tag_of(k), mid), v))
                .collect(),
            system_sim_time_s: st
                .system_sim_time_s
                .iter()
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
    active_scanner_probes: HashMap<i64, ActiveScannerProbe>,
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
        let cur_inst_key = ctx.instance_id as usize;
        let tactical_state = {
            let tac = ctx.tactical_state.read().unwrap_or_else(|e| e.into_inner());
            PersistedTacticalModulesPayload::from_state(&tac, cur_inst_key)
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
        let mut fighter_cooldown_state: Vec<_> = ctx
            .fighter_cooldown_state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(&k, &v)| (k, v))
            .collect();
        fighter_cooldown_state.sort_by_key(|(k, _)| *k);
        let mut fighter_charge_state: Vec<_> = ctx
            .fighter_charge_state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(&k, &v)| (k, v))
            .collect();
        fighter_charge_state.sort_by_key(|(k, _)| *k);
        let mut fighter_tube_loads: Vec<_> = ctx
            .fighter_tube_loads
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(&k, &v)| (k, v))
            .collect();
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

        Self {
            active_scanner_probes: ctx
                .active_scanner_probes
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            active_moon_probes: ctx
                .active_moon_probes
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            scanned_signatures,
            character_scan_progress,
            active_fighters: ctx
                .active_fighters
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            fighter_cooldown_state,
            fighter_charge_state,
            fighter_tube_loads,
            module_reactivation_cooldowns,
            mining_cycles: ctx
                .mining_cycles
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            phased_asteroid_fields: ctx
                .phased_asteroid_fields
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            session_change_timers: ctx
                .session_change_timers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            active_warps: ctx
                .active_warps
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            location_change_invuln: ctx
                .location_change_invuln
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            weapon_cycle_state: ctx
                .weapon_cycle_state
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            projected_ewar: ctx
                .projected_ewar
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            projected_ewar_sources: ctx
                .projected_ewar_sources
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            active_ewar_starts,
            map_landmarks: ctx
                .map_landmarks
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            tactical_state,
            fleet_extras_runtime,
            social_extras_runtime,
        }
    }

    fn is_empty(&self) -> bool {
        self.active_scanner_probes.is_empty()
            && self.active_moon_probes.is_empty()
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
        *ctx.active_scanner_probes
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.active_scanner_probes;
        *ctx.active_moon_probes
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.active_moon_probes;
        *ctx.scanned_signatures
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.scanned_signatures.into_iter().collect();
        *ctx.character_scan_progress
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.character_scan_progress.into_iter().collect();
        *ctx.active_fighters
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.active_fighters;
        *ctx.fighter_cooldown_state
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.fighter_cooldown_state.into_iter().collect();
        *ctx.fighter_charge_state
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.fighter_charge_state.into_iter().collect();
        *ctx.fighter_tube_loads
            .write()
            .unwrap_or_else(|e| e.into_inner()) = self.fighter_tube_loads.into_iter().collect();
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
        let (blob_opt, global_cfg, notifications) = self.db.with_snapshot(|snap| {
            let blob = snap
                .service_state_blobs
                .get(SERVER_RUNTIME_STATE_BLOB_KEY)
                .cloned();
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
            (blob, gcfg, notifs)
        });
        *self
            .global_config_values
            .write()
            .unwrap_or_else(|e| e.into_inner()) = global_cfg;
        *self
            .notification_records
            .write()
            .unwrap_or_else(|e| e.into_inner()) = notifications;

        let state = blob_opt
            .and_then(|s| serde_json::from_str::<PersistedServerRuntimeState>(&s).ok())
            .unwrap_or_default();
        state.apply_to_ctx(self);
    }

    /// Persists all gameplay-affecting `ServerContext` runtime maps into
    /// `WorldStateSnapshot::service_state_blobs` (`#592`).
    pub fn persist_server_runtime_to_db(&self) {
        if self.db.is_write_locked_by_current_thread() {
            return;
        }
        let payload = PersistedServerRuntimeState::from_ctx(self);
        if payload.is_empty() {
            let has_blob = self
                .db
                .with_snapshot(|s| s.service_state_blobs.contains_key(SERVER_RUNTIME_STATE_BLOB_KEY));
            if !has_blob {
                return;
            }
            let _ = self.db.with_mut_snapshot(|snap| {
                snap.service_state_blobs.remove(SERVER_RUNTIME_STATE_BLOB_KEY);
            });
            return;
        }
        if let Ok(json) = serde_json::to_string(&payload) {
            let _ = self.db.with_mut_snapshot(|snap| {
                snap.service_state_blobs
                    .insert(SERVER_RUNTIME_STATE_BLOB_KEY.to_string(), json);
            });
        }
    }

    /// Flushes all gameplay-affecting runtime state across `ServerContext`, `CombatRuntimeState`,
    /// social extras, and `QuasarGateway` into SQLite (`#592`).
    pub fn sync_runtime_to_db(&self) {
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
        sync_combat_runtime_from_db(self);
        self.quasar_gateway.rehydrate_all_from_db();
    }
}
