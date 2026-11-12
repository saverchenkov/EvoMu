pub mod entity_kv;
pub mod progression_seasons;
pub mod pve_sites;
pub mod pvp_tourney_deployables;
pub mod quasar_and_social;

pub use entity_kv::*;
pub use progression_seasons::*;
pub use pve_sites::*;
pub use pvp_tourney_deployables::*;
pub use quasar_and_social::*;

use crate::snapshot_ext::WorldStateSnapshot;
use eve_proto::PyRep;
use eve_sde::SolarSystemId;
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap, HashSet};

/// Maximum number of small opaque metadata entries allowed in `service_state_blobs`.
pub const MAX_SERVICE_STATE_BLOB_KEYS: usize = 256;
/// Maximum byte length of a single value stored in `service_state_blobs`.
pub const MAX_SERVICE_STATE_BLOB_VALUE_BYTES: usize = 4096;
/// Maximum total byte size across all keys and values in `service_state_blobs`.
pub const MAX_SERVICE_STATE_BLOB_TOTAL_BYTES: usize = 65_536;

static SERVICE_STATE_BLOB_LIMIT_VIOLATIONS_TOTAL: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Returns the cumulative count of `service_state_blobs` limit violations observed at write or commit time.
pub fn service_state_blob_limit_violations_total() -> u64 {
    SERVICE_STATE_BLOB_LIMIT_VIOLATIONS_TOTAL.load(std::sync::atomic::Ordering::Relaxed)
}

/// Logs a `service_state_blobs` limit violation and increments the violation metric counter.
pub fn record_service_state_blob_limit_violation(err: &str) {
    SERVICE_STATE_BLOB_LIMIT_VIOLATIONS_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    tracing::error!(error = %err, "service_state_blobs limit violation");
}

/// Validates a single write `(key, value)` against `service_state_blobs` limits without failing
/// because of an unrelated pre-existing oversized entry in `blobs`.
pub fn validate_service_state_blob_write(
    blobs: &HashMap<String, String>,
    key: &str,
    value: &str,
) -> Result<(), String> {
    if value.len() > MAX_SERVICE_STATE_BLOB_VALUE_BYTES {
        return Err(format!(
            "service_state_blobs key '{key}' value length {} exceeds limit {MAX_SERVICE_STATE_BLOB_VALUE_BYTES}",
            value.len()
        ));
    }
    let is_existing = blobs.contains_key(key);
    let new_key_count = if is_existing {
        blobs.len()
    } else {
        blobs.len().saturating_add(1)
    };
    if new_key_count > MAX_SERVICE_STATE_BLOB_KEYS && !is_existing {
        return Err(format!(
            "service_state_blobs key count {new_key_count} exceeds limit {MAX_SERVICE_STATE_BLOB_KEYS}"
        ));
    }
    let current_total: usize = blobs
        .iter()
        .map(|(k, v)| k.len().saturating_add(v.len()))
        .sum();
    let old_entry_bytes = blobs
        .get(key)
        .map(|v| key.len().saturating_add(v.len()))
        .unwrap_or(0);
    let new_entry_bytes = key.len().saturating_add(value.len());
    let total_bytes = current_total
        .saturating_sub(old_entry_bytes)
        .saturating_add(new_entry_bytes);
    if total_bytes > MAX_SERVICE_STATE_BLOB_TOTAL_BYTES && new_entry_bytes >= old_entry_bytes {
        return Err(format!(
            "service_state_blobs total size {total_bytes} exceeds limit {MAX_SERVICE_STATE_BLOB_TOTAL_BYTES}"
        ));
    }
    Ok(())
}

/// Validates that `service_state_blobs` strictly holds bounded small opaque metadata.
pub fn validate_service_state_blobs(blobs: &HashMap<String, String>) -> Result<(), String> {
    if blobs.len() > MAX_SERVICE_STATE_BLOB_KEYS {
        return Err(format!(
            "service_state_blobs key count {} exceeds limit {}",
            blobs.len(),
            MAX_SERVICE_STATE_BLOB_KEYS
        ));
    }
    let mut total_bytes = 0usize;
    for (k, v) in blobs {
        if v.len() > MAX_SERVICE_STATE_BLOB_VALUE_BYTES {
            return Err(format!(
                "service_state_blobs key '{k}' value length {} exceeds limit {MAX_SERVICE_STATE_BLOB_VALUE_BYTES}",
                v.len()
            ));
        }
        total_bytes = total_bytes.saturating_add(k.len()).saturating_add(v.len());
    }
    if total_bytes > MAX_SERVICE_STATE_BLOB_TOTAL_BYTES {
        return Err(format!(
            "service_state_blobs total size {total_bytes} exceeds limit {MAX_SERVICE_STATE_BLOB_TOTAL_BYTES}"
        ));
    }
    Ok(())
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct LegacyPveSpecialSitesCombined {
    initialized: bool,
    incursions: HashMap<ConstellationId, IncursionTaleRecord>,
    incursion_lifecycle: HashMap<ConstellationId, IncursionLifecycleMeta>,
    incursion_site_waves: HashMap<String, IncursionSiteWaveMeta>,
    world_event_tales: HashMap<i64, WorldEventTaleRecord>,
    incursion_lp_logs: HashMap<i64, Vec<IncursionLPLogEntry>>,
    abyssal_pockets: HashMap<i64, AbyssalPocketSession>,
    abyssal_traces: HashMap<i64, AbyssalTraceRecord>,
    void_space_sessions: HashMap<i64, VoidSpaceSessionRecord>,
    homefront_sites: HashMap<SolarSystemId, Vec<HomefrontSiteRecord>>,
    roaming_storms: Vec<RoamingStormRecord>,
    drifter_hives: HashMap<SolarSystemId, DrifterHiveRecord>,
    triglavian_sites: HashMap<SolarSystemId, Vec<TriglavianSiteRecord>>,
    edencom_fortress_systems: Vec<SolarSystemId>,
    edencom_minor_victory_systems: Vec<SolarSystemId>,
    triglavian_minor_victory_systems: Vec<SolarSystemId>,
    interference_by_system: HashMap<SolarSystemId, SystemInterferenceRecord>,
    crab_beacons: HashMap<i64, CrabBeaconRecord>,
    seasonal_event_sites: HashMap<SolarSystemId, Vec<SeasonalEventSiteRecord>>,
    sim_clock_sec: u64,
    next_seq: i64,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LegacyFlaggedSite {
    Structured(FlaggedSiteEntry),
    LegacyUrl(String),
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct LegacyBrowserLockdownPolicy {
    lockdown: bool,
    default_home_page: String,
    flagged_sites: Vec<LegacyFlaggedSite>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LegacyVisitRecord {
    Full(SolarSystemVisitRecord),
    LegacyCount(i64),
}

/// Migrates legacy serialized domain stores stored inside `service_state_blobs` and sentinel
/// `owner_notes` entries onto the first-class typed fields on `WorldStateSnapshot`, removing the
/// superseded legacy entries so `service_state_blobs` contains only bounded opaque metadata.
pub fn migrate_legacy_service_state(snap: &mut WorldStateSnapshot) {
    // 1. Sentinel owner_notes entries -> typed snapshot fields
    const LEGACY_PVE_SPECIAL_NOTE_ID: i64 = -8_300_000_000_000;
    const LEGACY_TOURNEY_NOTE_ID: i64 = -10_000_055;
    const LEGACY_DEPLOYABLE_NOTE_ID: i64 = -8_650_000_000_000;
    const LEGACY_PVP_FILAMENT_NOTE_ID: i64 = -990_050;

    if let Some(incursion_raw) = snap
        .service_state_blobs
        .remove("incursions_state")
        .or_else(|| snap.owner_notes.remove(&LEGACY_PVE_SPECIAL_NOTE_ID).map(|n| n.note))
    {
        if let Ok(legacy) = serde_json::from_str::<LegacyPveSpecialSitesCombined>(&incursion_raw) {
            if !snap.pve_special_sites.initialized
                && (legacy.initialized
                    || !legacy.incursions.is_empty()
                    || !legacy.homefront_sites.is_empty()
                    || !legacy.roaming_storms.is_empty()
                    || !legacy.drifter_hives.is_empty()
                    || !legacy.triglavian_sites.is_empty()
                    || !legacy.seasonal_event_sites.is_empty()
                    || !legacy.interference_by_system.is_empty()
                    || !legacy.world_event_tales.is_empty()
                    || !legacy.incursion_lp_logs.is_empty()
                    || legacy.next_seq > 0)
            {
                snap.pve_special_sites = PveSpecialSitesState {
                    initialized: legacy.initialized,
                    incursions: legacy.incursions,
                    incursion_lifecycle: legacy.incursion_lifecycle,
                    incursion_site_waves: legacy.incursion_site_waves,
                    world_event_tales: legacy.world_event_tales,
                    incursion_lp_logs: legacy.incursion_lp_logs,
                    homefront_sites: legacy.homefront_sites,
                    roaming_storms: legacy.roaming_storms,
                    drifter_hives: legacy.drifter_hives,
                    triglavian_sites: legacy.triglavian_sites,
                    edencom_fortress_systems: legacy.edencom_fortress_systems,
                    edencom_minor_victory_systems: legacy.edencom_minor_victory_systems,
                    triglavian_minor_victory_systems: legacy.triglavian_minor_victory_systems,
                    interference_by_system: legacy.interference_by_system,
                    seasonal_event_sites: legacy.seasonal_event_sites,
                    sim_clock_sec: legacy.sim_clock_sec,
                    next_seq: legacy.next_seq,
                };
            }
            if snap.crab_beacons.beacons.is_empty() && !legacy.crab_beacons.is_empty() {
                snap.crab_beacons.beacons = legacy.crab_beacons;
            }
            if snap.abyssal_state.pockets.is_empty() && !legacy.abyssal_pockets.is_empty() {
                snap.abyssal_state.pockets = legacy.abyssal_pockets;
            }
            if snap.abyssal_state.traces.is_empty() && !legacy.abyssal_traces.is_empty() {
                snap.abyssal_state.traces = legacy.abyssal_traces;
            }
            if snap.abyssal_state.void_space_sessions.is_empty()
                && !legacy.void_space_sessions.is_empty()
            {
                snap.abyssal_state.void_space_sessions = legacy.void_space_sessions;
            }
            if snap.abyssal_state.next_seq < legacy.next_seq {
                snap.abyssal_state.next_seq = legacy.next_seq;
            }
        }
    } else {
        snap.owner_notes.remove(&LEGACY_PVE_SPECIAL_NOTE_ID);
    }

    if let Some(note) = snap.owner_notes.remove(&LEGACY_TOURNEY_NOTE_ID) {
        if snap.tourney_state.matches.is_empty()
            && snap.tourney_state.tournaments.is_empty()
            && snap.tourney_state.arena_systems.is_empty()
        {
            if let Ok(parsed) = serde_json::from_str::<TourneyStore>(&note.note) {
                snap.tourney_state = parsed;
            }
        }
    }

    if let Some(note) = snap.owner_notes.remove(&LEGACY_DEPLOYABLE_NOTE_ID) {
        if snap.deployable_state.deployables.is_empty()
            && snap.deployable_state.damaged_items.is_empty()
            && snap.deployable_state.mmju_spools.is_empty()
        {
            if let Ok(parsed) = serde_json::from_str::<MobileDeployableRuntimeState>(&note.note) {
                snap.deployable_state = parsed;
            }
        }
    }

    if let Some(raw) = snap
        .service_state_blobs
        .remove("pvp_filament_store")
        .or_else(|| snap.service_state_blobs.remove("pvp_filament:global"))
        .or_else(|| snap.owner_notes.remove(&LEGACY_PVP_FILAMENT_NOTE_ID).map(|n| n.note))
    {
        if snap.pvp_filament_state.queue.is_empty()
            && snap.pvp_filament_state.active_matches.is_empty()
            && snap.pvp_filament_state.events.is_empty()
        {
            if let Ok(parsed) = serde_json::from_str::<PvpFilamentStore>(&raw) {
                snap.pvp_filament_state = parsed;
            }
        }
    } else {
        snap.service_state_blobs.remove("pvp_filament:global");
        snap.owner_notes.remove(&LEGACY_PVP_FILAMENT_NOTE_ID);
    }

    // 2. Single-key structured domain blobs -> typed fields
    if let Some(raw) = snap.service_state_blobs.remove("abyssal_active_clouds") {
        if snap.abyssal_state.active_clouds.is_empty() {
            if let Ok(parsed) = serde_json::from_str::<HashMap<i64, u32>>(&raw) {
                snap.abyssal_state.active_clouds = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("abyssal_boundary_ticks") {
        if snap.abyssal_state.boundary_ticks.is_empty() {
            if let Ok(parsed) = serde_json::from_str::<HashMap<i64, u32>>(&raw) {
                snap.abyssal_state.boundary_ticks = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("deathzone_active_ships") {
        if snap.abyssal_state.deathzone_active_ships.is_empty() {
            if let Ok(parsed) = serde_json::from_str::<BTreeSet<i64>>(&raw) {
                snap.abyssal_state.deathzone_active_ships = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("dungeon_runtime_store") {
        if snap.dungeon_runtime.instances.is_empty() {
            if let Ok(parsed) = serde_json::from_str::<DungeonRuntimeStore>(&raw) {
                snap.dungeon_runtime = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("dungeon_editor_store") {
        if snap.dungeon_editor.rooms_by_dungeon.is_empty()
            && snap.dungeon_editor.active_editors.is_empty()
        {
            if let Ok(parsed) = serde_json::from_str::<DungeonEditorStore>(&raw) {
                snap.dungeon_editor = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("aoe_superweapon_runtime") {
        if snap.superweapon_runtime.active_cycles.is_empty()
            && snap.superweapon_runtime.warp_suppressions.is_empty()
        {
            if let Ok(parsed) = serde_json::from_str::<PersistedSuperweaponRuntime>(&raw) {
                snap.superweapon_runtime = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("insurgency_complex_ops") {
        if snap.insurgency_complex_ops.outpost_raids.is_empty()
            && snap.insurgency_complex_ops.mining_ambushes.is_empty()
        {
            if let Ok(mut parsed) = serde_json::from_str::<InsurgencyComplexOpsState>(&raw) {
                if parsed.ice_heist_respawn_until.is_empty() {
                    parsed.ice_heist_respawn_until =
                        std::mem::take(&mut snap.insurgency_complex_ops.ice_heist_respawn_until);
                }
                snap.insurgency_complex_ops = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("phased_asteroid_fields") {
        if snap.mining_and_ess_state.phased_asteroid_fields.is_empty() {
            if let Ok(parsed) =
                serde_json::from_str::<HashMap<u32, PhasedAsteroidFieldState>>(&raw)
            {
                snap.mining_and_ess_state.phased_asteroid_fields = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("phased_field_last_tick_unix") {
        if snap.mining_and_ess_state.phased_field_last_tick_unix == 0 {
            if let Ok(v) = raw.parse::<u64>() {
                snap.mining_and_ess_state.phased_field_last_tick_unix = v;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("moon_fractured_fields") {
        if snap.mining_and_ess_state.moon_fractured_fields.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.mining_and_ess_state.moon_fractured_fields = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("mining_ledger_waste") {
        if snap.mining_and_ess_state.mining_ledger_waste.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.mining_and_ess_state.mining_ledger_waste = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("metenox_last_tick_unix") {
        if snap.mining_and_ess_state.metenox_last_tick_unix == 0 {
            if let Ok(v) = raw.parse::<u64>() {
                snap.mining_and_ess_state.metenox_last_tick_unix = v;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("client_node_graphs:seq") {
        if snap.node_graph_state.client_node_graph_seq == 0 {
            if let Ok(v) = raw.parse::<i64>() {
                snap.node_graph_state.client_node_graph_seq = v;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("login_campaign_enrollments") {
        if snap.seasons_and_campaigns.login_campaign_enrollments.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.seasons_and_campaigns.login_campaign_enrollments = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("daily_login_campaign_schedule") {
        if snap.seasons_and_campaigns.daily_login_campaign_schedule.is_none() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.seasons_and_campaigns.daily_login_campaign_schedule = Some(parsed);
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("seasonal_login_schedule") {
        if snap.seasons_and_campaigns.seasonal_login_schedule.is_none() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.seasons_and_campaigns.seasonal_login_schedule = Some(parsed);
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("project_discovery") {
        if snap.seasons_and_campaigns.project_discovery_store.tasks.is_empty()
            && !snap.seasons_and_campaigns.project_discovery_store.mmos_configured
        {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.seasons_and_campaigns.project_discovery_store = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("project_discovery_votes") {
        if snap.seasons_and_campaigns.project_discovery_votes.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.seasons_and_campaigns.project_discovery_votes = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("season_manager") {
        if let Ok(parsed) = serde_json::from_str::<HashMap<i64, CharacterSeasonState>>(&raw) {
            for (cid, st) in parsed {
                snap.seasons_and_campaigns.season_states.entry(cid).or_insert(st);
            }
        }
    }

    // Quasar commerce
    if let Some(raw) = snap.service_state_blobs.remove("payment_tokens") {
        if snap.quasar_commerce.payment_tokens.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.quasar_commerce.payment_tokens = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("payment_receipts") {
        if snap.quasar_commerce.payment_receipts.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.quasar_commerce.payment_receipts = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("payment_schedules") {
        if snap.quasar_commerce.payment_schedules.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.quasar_commerce.payment_schedules = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("plex_tx_logs") {
        if snap.quasar_commerce.plex_tx_logs.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.quasar_commerce.plex_tx_logs = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("nes_store_catalog") {
        if snap.quasar_commerce.nes_store_catalog.is_none() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.quasar_commerce.nes_store_catalog = Some(parsed);
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("nes_store_offer_tags") {
        if snap.quasar_commerce.nes_store_offer_tags.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.quasar_commerce.nes_store_offer_tags = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("plex_invoices") {
        if snap.quasar_commerce.plex_invoices.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.quasar_commerce.plex_invoices = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("quasar_cosmetics_market") {
        if snap.quasar_commerce.cosmetics_market.is_none() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.quasar_commerce.cosmetics_market = Some(parsed);
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("content_orchestrator_state") {
        if snap.quasar_commerce.orchestrator.deployments.is_empty()
            && snap.quasar_commerce.orchestrator.instances.is_empty()
        {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.quasar_commerce.orchestrator = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("store_failed_fulfillments") {
        if snap.quasar_commerce.failed_fulfillments.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.quasar_commerce.failed_fulfillments = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("store_targeted_offers") {
        if snap.quasar_commerce.targeted_offers.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.quasar_commerce.targeted_offers = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("quasar_acl_registry") {
        if snap.quasar_commerce.acl_registry.is_none() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.quasar_commerce.acl_registry = Some(parsed);
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("b34_lp_assetholding_ops") {
        if let Ok(parsed) = serde_json::from_str::<HashSet<String>>(&raw) {
            snap.quasar_commerce.lp_asset_ops_completed.extend(parsed);
        }
    }

    // Fleet & social
    if let Some(raw) = snap.service_state_blobs.remove("fleet_saved_setups") {
        if snap.fleet_and_social_ops.fleet_saved_setups.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.fleet_and_social_ops.fleet_saved_setups = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("fleet_stored_formations") {
        if snap.fleet_and_social_ops.fleet_stored_formations.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.fleet_and_social_ops.fleet_stored_formations = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("fleet_adverts") {
        if snap.fleet_and_social_ops.fleet_adverts.is_empty() {
            if let Ok(parsed) = serde_json::from_str::<HashMap<String, FleetAdvertRecord>>(&raw) {
                for (k, v) in parsed {
                    if let Ok(fid) = k.parse::<i64>() {
                        snap.fleet_and_social_ops.fleet_adverts.insert(fid, v);
                    }
                }
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("fleet_join_requests") {
        if snap.fleet_and_social_ops.fleet_join_requests.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.fleet_and_social_ops.fleet_join_requests = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("fleet_watchlist") {
        if snap.fleet_and_social_ops.fleet_watchlist.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.fleet_and_social_ops.fleet_watchlist = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("fleet_options") {
        if snap.fleet_and_social_ops.fleet_voice_enabled.is_empty()
            && snap.fleet_and_social_ops.fleet_loot_logging.is_empty()
        {
            if let Ok((v, l)) =
                serde_json::from_str::<(HashMap<i64, bool>, HashMap<i64, bool>)>(&raw)
            {
                snap.fleet_and_social_ops.fleet_voice_enabled = v;
                snap.fleet_and_social_ops.fleet_loot_logging = l;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("corp_recruitment_ads") {
        if snap.fleet_and_social_ops.corp_recruitment_ads.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.fleet_and_social_ops.corp_recruitment_ads = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("corp_allow_war") {
        if snap.fleet_and_social_ops.corp_allow_war.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.fleet_and_social_ops.corp_allow_war = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("hypernet_subs") {
        if snap.fleet_and_social_ops.hypernet_subs.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.fleet_and_social_ops.hypernet_subs = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("ingamereport_tickets") {
        if snap.fleet_and_social_ops.ingamereport_tickets.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.fleet_and_social_ops.ingamereport_tickets = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("lp_store_offers") {
        if snap.fleet_and_social_ops.lp_store_offers.is_none() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.fleet_and_social_ops.lp_store_offers = Some(parsed);
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("agentMgr:disabled_missions") {
        if let Ok(parsed) = serde_json::from_str::<Vec<u32>>(&raw) {
            snap.fleet_and_social_ops
                .agent_global_disabled_missions
                .extend(parsed);
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("pending_probe_scans") {
        if snap.fleet_and_social_ops.pending_probe_scans.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.fleet_and_social_ops.pending_probe_scans = parsed;
            }
        }
    }

    // Cluster & runtime
    if let Some(raw) = snap.service_state_blobs.remove("cluster_misc_state") {
        if snap.cluster_and_runtime.cluster_misc_state.is_none() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.cluster_misc_state = Some(parsed);
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("clone_vat_bay_state") {
        if snap.cluster_and_runtime.clone_vat_bay_state.active_by_ship.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.clone_vat_bay_state = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("load_service_state") {
        if !snap.cluster_and_runtime.load_service_state.active
            && snap.cluster_and_runtime.load_service_state.runs_started == 0
        {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.load_service_state = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("active_sessions") {
        if snap.cluster_and_runtime.active_sessions.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.active_sessions = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("session_registry") {
        if snap.cluster_and_runtime.session_registry.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.session_registry = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("cluster_node_table") {
        if snap.cluster_and_runtime.cluster_node_table.is_none() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.cluster_node_table = Some(parsed);
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("cluster_admin_broadcasts") {
        if snap.cluster_and_runtime.cluster_admin_broadcasts.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.cluster_admin_broadcasts = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("cluster_service_states") {
        if snap.cluster_and_runtime.cluster_service_states.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.cluster_service_states = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("cluster_attacks") {
        if snap.cluster_and_runtime.cluster_attacks.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.cluster_attacks = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("external_queue_events") {
        if snap.cluster_and_runtime.external_queue_events.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.external_queue_events = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("cluster_downtime_state") {
        if snap.cluster_and_runtime.cluster_downtime_state.cluster_restart_epoch == 0
            && snap.cluster_and_runtime.cluster_downtime_state.last_maintenance_unix == 0
        {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.cluster_downtime_state = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("macho_client_code_hash") {
        if snap.cluster_and_runtime.macho_client_code_hash.is_none() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.macho_client_code_hash = Some(parsed);
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("macho_global_config") {
        if snap.cluster_and_runtime.macho_global_config.is_empty() {
            if let Ok(parsed) = serde_json::from_str::<HashMap<String, PyRep>>(&raw) {
                snap.cluster_and_runtime.macho_global_config = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("user_sessions") {
        if snap.cluster_and_runtime.user_sessions.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.user_sessions = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("gm_audit_log") {
        if snap.cluster_and_runtime.gm_audit_log.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.gm_audit_log = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("alert_bean_counts") {
        if snap.cluster_and_runtime.alert_bean_counts.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.alert_bean_counts = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("alert_log_modes") {
        if snap.cluster_and_runtime.alert_log_modes.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.alert_log_modes = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("alert_client_traces") {
        if snap.cluster_and_runtime.alert_client_traces.is_none() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.alert_client_traces = Some(parsed);
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("alert_proxy_traces") {
        if snap.cluster_and_runtime.alert_proxy_traces.is_none() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.alert_proxy_traces = Some(parsed);
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("alert_group_bean_bytes") {
        if snap.cluster_and_runtime.alert_group_bean_bytes.is_none() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.alert_group_bean_bytes = Some(parsed);
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("browser_lockdown_policy") {
        if snap.cluster_and_runtime.browser_lockdown_policy.is_none() {
            if let Ok(parsed) = serde_json::from_str::<LegacyBrowserLockdownPolicy>(&raw) {
                snap.cluster_and_runtime.browser_lockdown_policy = Some(BrowserLockdownPolicyState {
                    lockdown: parsed.lockdown,
                    default_home_page: parsed.default_home_page,
                    flagged_sites: parsed
                        .flagged_sites
                        .into_iter()
                        .map(|s| match s {
                            LegacyFlaggedSite::Structured(e) => e,
                            LegacyFlaggedSite::LegacyUrl(site_url) => FlaggedSiteEntry {
                                site_url,
                                site_flag: 2,
                            },
                        })
                        .collect(),
                });
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("localization:qa_wrap_users") {
        if snap.cluster_and_runtime.localization_qa_wrap_users.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.localization_qa_wrap_users = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("client_stat_logger:strings") {
        if snap.cluster_and_runtime.client_stat_logger_strings.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.client_stat_logger_strings = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("blacklisted_planets") {
        if snap.cluster_and_runtime.blacklisted_planets.is_none() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.blacklisted_planets = Some(parsed);
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("module_reload_until_unix") {
        if snap.cluster_and_runtime.module_reload_until_unix.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.module_reload_until_unix = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("warp_vector_cooldown_until") {
        if snap.cluster_and_runtime.warp_vector_cooldown_until.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.warp_vector_cooldown_until = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("jumpCloneSvc:ship_clone_offers") {
        if snap.cluster_and_runtime.ship_clone_offers.is_empty() {
            if let Ok(parsed) = serde_json::from_str(&raw) {
                snap.cluster_and_runtime.ship_clone_offers = parsed;
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("account_resculpt_tokens") {
        if let Ok(parsed) = serde_json::from_str::<HashMap<String, u32>>(&raw) {
            for (k, v) in parsed {
                snap.cluster_and_runtime
                    .account_resculpt_tokens
                    .entry(k)
                    .or_insert(v);
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("server_runtime_state") {
        if snap.cluster_and_runtime.server_runtime_json.is_none() {
            snap.cluster_and_runtime.server_runtime_json = Some(raw);
        }
    }

    // 3. Prefixed keys -> typed maps
    let prefixed_keys: Vec<String> = snap
        .service_state_blobs
        .keys()
        .filter(|k| {
            k.starts_with("observatory_site:")
                || k.starts_with("observatory_respawn_until:")
                || k.starts_with("fob_combatants:")
                || k.starts_with("fob_faction:")
                || k.starts_with("fob_respawn_until:")
                || k.starts_with("fob_defense_triggered:")
                || k.starts_with("ice_heist_respawn_until:")
                || k.starts_with("ess_reserve_pulses:")
                || k.starts_with("void_space_content:")
                || k.starts_with("air_npe_runner:")
                || k.starts_with("client_node_graphs:")
                || k.starts_with("node_graph_bb:")
                || k.starts_with("expert_systems:")
                || k.starts_with("character_certificates:")
                || k.starts_with("char_certs:")
                || k.starts_with("season_state:")
                || k.starts_with("season_manager:")
                || k.starts_with("active_season:")
                || k.starts_with("hacking_wins:")
                || k.starts_with("milestone_playtime:")
                || k.starts_with("auto_injected_rewards:")
                || k.starts_with("login_reward_expiry:")
                || k.starts_with("project_discovery:")
                || k.starts_with("activity_mgr:")
                || k.starts_with("fast_checkout:")
                || k.starts_with("contract_deposit:")
                || k.starts_with("delayed_rewards:")
                || k.starts_with("mission_taxed_journal:")
                || k.starts_with("agentMgr:disabled_missions:")
                || k.starts_with("agentMgr:replay_ts:")
                || k.starts_with("signal_tracker_sub:")
                || k.starts_with("wh_local_witnesses:")
                || k.starts_with("structure_defense:")
                || k.starts_with("alliance_sov_cfg:")
                || k.starts_with("portrait:")
                || k.starts_with("paperdoll_state:")
                || k.starts_with("char_system_visits:")
                || k.starts_with("auto_reloaded_event:")
                || k.starts_with("mct_slots:")
                || k.starts_with("account_resculpt_tokens:")
                || k.starts_with("resculpt_pending:")
                || k.starts_with("char_cohorts:")
                || k.starts_with("user_cohorts:")
                || k.starts_with("character_lock:")
                || k.starts_with("omega_downgraded:")
                || k.starts_with("character_medals:")
                || k.starts_with("char_medals:")
        })
        .cloned()
        .collect();

    for key in prefixed_keys {
        let Some(val) = snap.service_state_blobs.remove(&key) else {
            continue;
        };
        if let Some(id_str) = key.strip_prefix("observatory_site:") {
            if let (Ok(id), Ok(st)) = (id_str.parse::<i64>(), serde_json::from_str(&val)) {
                snap.fob_observatory_state
                    .observatory_sites
                    .entry(id)
                    .or_insert(st);
            }
        } else if let Some(id_str) = key.strip_prefix("observatory_respawn_until:") {
            if let (Ok(id), Ok(ts)) = (id_str.parse::<u32>(), val.parse::<u64>()) {
                snap.fob_observatory_state
                    .observatory_respawn_until
                    .entry(id)
                    .or_insert(ts);
            }
        } else if let Some(id_str) = key.strip_prefix("fob_combatants:") {
            if let (Ok(id), Ok(set)) = (
                id_str.parse::<i64>(),
                serde_json::from_str::<BTreeSet<i64>>(&val),
            ) {
                snap.fob_observatory_state
                    .fob_combatants
                    .entry(id)
                    .or_insert(set);
            }
        } else if let Some(id_str) = key.strip_prefix("fob_faction:") {
            if let (Ok(id), Ok(fid)) = (id_str.parse::<i64>(), val.parse::<i64>()) {
                snap.fob_observatory_state
                    .fob_factions
                    .entry(id)
                    .or_insert(fid);
            }
        } else if let Some(id_str) = key.strip_prefix("fob_respawn_until:") {
            if let (Ok(id), Ok(ts)) = (id_str.parse::<u32>(), val.parse::<u64>()) {
                snap.fob_observatory_state
                    .fob_respawn_until
                    .entry(id)
                    .or_insert(ts);
            }
        } else if let Some(id_str) = key.strip_prefix("fob_defense_triggered:") {
            if let Ok(id) = id_str.parse::<i64>() {
                snap.fob_observatory_state.fob_defense_triggered.insert(id);
            }
        } else if let Some(id_str) = key.strip_prefix("ice_heist_respawn_until:") {
            if let (Ok(id), Ok(ts)) = (id_str.parse::<u32>(), val.parse::<u64>()) {
                snap.insurgency_complex_ops
                    .ice_heist_respawn_until
                    .entry(id)
                    .or_insert(ts);
            }
        } else if let Some(id_str) = key.strip_prefix("ess_reserve_pulses:") {
            if let (Ok(id), Ok(st)) = (id_str.parse::<u32>(), serde_json::from_str(&val)) {
                snap.mining_and_ess_state
                    .ess_reserve_pulses
                    .entry(id)
                    .or_insert(st);
            }
        } else if let Some(id_str) = key.strip_prefix("void_space_content:") {
            if let (Ok(id), Ok(cid)) = (id_str.parse::<i64>(), val.parse::<i64>()) {
                snap.abyssal_state
                    .void_space_content
                    .entry(id)
                    .or_insert(cid);
            }
        } else if let Some(id_str) = key.strip_prefix("air_npe_runner:") {
            if let (Ok(id), Ok(st)) = (id_str.parse::<i64>(), serde_json::from_str(&val)) {
                snap.node_graph_state.air_npe_runners.entry(id).or_insert(st);
            }
        } else if let Some(id_str) = key.strip_prefix("client_node_graphs:") {
            if let (Ok(id), Ok(st)) = (id_str.parse::<i64>(), serde_json::from_str(&val)) {
                snap.node_graph_state
                    .client_node_graphs
                    .entry(id)
                    .or_insert(st);
            }
        } else if let Some(rest) = key.strip_prefix("node_graph_bb:") {
            if let Some((c_str, g_str)) = rest.split_once(':') {
                if let (Ok(cid), Ok(gid), Ok(bb)) = (
                    c_str.parse::<i64>(),
                    g_str.parse::<i64>(),
                    serde_json::from_str(&val),
                ) {
                    snap.node_graph_state
                        .node_graph_blackboards
                        .entry((cid, gid))
                        .or_insert(bb);
                }
            }
        } else if let Some(id_str) = key.strip_prefix("expert_systems:") {
            if let (Ok(id), Ok(st)) = (id_str.parse::<i64>(), serde_json::from_str(&val)) {
                snap.node_graph_state.expert_systems.entry(id).or_insert(st);
            }
        } else if let Some(id_str) = key.strip_prefix("character_certificates:") {
            if let (Ok(id), Ok(st)) = (id_str.parse::<i64>(), serde_json::from_str(&val)) {
                snap.node_graph_state.certificates.entry(id).or_insert(st);
            }
        } else if let Some(id_str) = key.strip_prefix("char_certs:") {
            if let (Ok(id), Ok(certs)) = (
                id_str.parse::<i64>(),
                serde_json::from_str::<HashMap<u32, u8>>(&val),
            ) {
                snap.node_graph_state
                    .certificates
                    .entry(id)
                    .or_insert_with(|| CharacterCertificateState {
                        certificates: certs,
                        flags: HashMap::new(),
                    });
            }
        } else if let Some(id_str) = key
            .strip_prefix("season_state:")
            .or_else(|| key.strip_prefix("season_manager:"))
        {
            match id_str {
                "season_configuration" => {
                    if snap.seasons_and_campaigns.season_configuration.is_none() {
                        if let Ok(cfg) = serde_json::from_str(&val) {
                            snap.seasons_and_campaigns.season_configuration = Some(cfg);
                        }
                    }
                }
                "seasons" => {
                    if snap.seasons_and_campaigns.seasons_catalog.is_empty() {
                        if let Ok(cat) = serde_json::from_str(&val) {
                            snap.seasons_and_campaigns.seasons_catalog = cat;
                        }
                    }
                }
                "challenge_definitions" => {
                    if snap.seasons_and_campaigns.challenge_definitions.is_empty() {
                        if let Ok(defs) = serde_json::from_str(&val) {
                            snap.seasons_and_campaigns.challenge_definitions = defs;
                        }
                    }
                }
                "loot_tables" => {
                    if snap.seasons_and_campaigns.season_loot_tables.is_empty() {
                        if let Ok(tbl) = serde_json::from_str(&val) {
                            snap.seasons_and_campaigns.season_loot_tables = tbl;
                        }
                    }
                }
                "announced_initialized" => {
                    snap.seasons_and_campaigns.announced_season_init = true;
                }
                "announced_states" => {
                    if snap.seasons_and_campaigns.announced_season_states.is_empty() {
                        if let Ok(st) = serde_json::from_str(&val) {
                            snap.seasons_and_campaigns.announced_season_states = st;
                        }
                    }
                }
                _ => {
                    if let (Ok(id), Ok(st)) = (id_str.parse::<i64>(), serde_json::from_str(&val)) {
                        snap.seasons_and_campaigns
                            .season_states
                            .entry(id)
                            .or_insert(st);
                    }
                }
            }
        } else if let Some(id_str) = key.strip_prefix("active_season:") {
            if let (Ok(id), Ok(sid)) = (id_str.parse::<i64>(), val.parse::<i64>()) {
                snap.seasons_and_campaigns
                    .active_seasons
                    .entry(id)
                    .or_insert(sid);
            }
        } else if let Some(id_str) = key.strip_prefix("hacking_wins:") {
            if let (Ok(id), Ok(wins)) = (id_str.parse::<i64>(), val.parse::<u64>()) {
                snap.seasons_and_campaigns
                    .hacking_wins
                    .entry(id)
                    .or_insert(wins);
            }
        } else if let Some(id_str) = key.strip_prefix("milestone_playtime:") {
            if let (Ok(id), Ok(st)) = (id_str.parse::<i64>(), serde_json::from_str(&val)) {
                snap.seasons_and_campaigns
                    .milestone_playtime
                    .entry(id)
                    .or_insert(st);
            }
        } else if let Some(id_str) = key.strip_prefix("auto_injected_rewards:") {
            if let (Ok(id), Ok(list)) = (id_str.parse::<i64>(), serde_json::from_str(&val)) {
                snap.seasons_and_campaigns
                    .auto_injected_rewards
                    .entry(id)
                    .or_insert(list);
            }
        } else if let Some(id_str) = key.strip_prefix("login_reward_expiry:") {
            if let (Ok(id), Ok(exp)) = (id_str.parse::<i64>(), val.parse::<i64>()) {
                snap.seasons_and_campaigns
                    .login_reward_expiry
                    .entry(id)
                    .or_insert(exp);
            }
        } else if let Some(id_str) = key.strip_prefix("project_discovery:") {
            if let (Ok(id), Ok(st)) = (id_str.parse::<i64>(), serde_json::from_str(&val)) {
                snap.seasons_and_campaigns
                    .project_discovery_players
                    .entry(id)
                    .or_insert(st);
            }
        } else if let Some(id_str) = key.strip_prefix("activity_mgr:") {
            if let (Ok(id), Ok(st)) = (id_str.parse::<i64>(), serde_json::from_str(&val)) {
                snap.seasons_and_campaigns
                    .character_activities
                    .entry(id)
                    .or_insert(st);
            }
        } else if let Some(id_str) = key.strip_prefix("fast_checkout:") {
            if let (Ok(id), Ok(st)) = (id_str.parse::<u64>(), serde_json::from_str(&val)) {
                snap.seasons_and_campaigns
                    .fast_checkout_states
                    .entry(id)
                    .or_insert(st);
            }
        } else if let Some(id_str) = key.strip_prefix("contract_deposit:") {
            if let (Ok(id), Ok(amt)) = (id_str.parse::<i64>(), val.parse::<i64>()) {
                snap.fleet_and_social_ops
                    .contract_deposits
                    .entry(id)
                    .or_insert(amt);
            }
        } else if let Some(id_str) = key.strip_prefix("delayed_rewards:") {
            if let (Ok(id), Ok(st)) = (id_str.parse::<i64>(), serde_json::from_str(&val)) {
                snap.fleet_and_social_ops
                    .delayed_rewards
                    .entry(id)
                    .or_insert(st);
            }
        } else if let Some(id_str) = key.strip_prefix("mission_taxed_journal:") {
            if let Ok(id) = id_str.parse::<i64>() {
                snap.fleet_and_social_ops.mission_taxed_journal.insert(id);
            }
        } else if let Some(id_str) = key.strip_prefix("agentMgr:disabled_missions:") {
            if let (Ok(id), Ok(list)) = (
                id_str.parse::<i64>(),
                serde_json::from_str::<Vec<u32>>(&val),
            ) {
                snap.fleet_and_social_ops
                    .agent_char_disabled_missions
                    .entry(id)
                    .or_default()
                    .extend(list);
            }
        } else if let Some(rest) = key.strip_prefix("agentMgr:replay_ts:") {
            if let Some((c_str, a_str)) = rest.split_once(':') {
                if let (Ok(cid), Ok(aid), Ok(ts)) =
                    (c_str.parse::<i64>(), a_str.parse::<i64>(), val.parse::<u64>())
                {
                    snap.fleet_and_social_ops
                        .agent_replay_ts
                        .entry((cid, aid))
                        .or_insert(ts);
                }
            }
        } else if let Some(id_str) = key.strip_prefix("signal_tracker_sub:") {
            if let (Ok(id), Ok(subs)) = (id_str.parse::<u32>(), serde_json::from_str(&val)) {
                snap.fleet_and_social_ops
                    .signal_tracker_subs
                    .entry(id)
                    .or_insert(subs);
            }
        } else if let Some(rest) = key.strip_prefix("wh_local_witnesses:") {
            if let Ok(set) = serde_json::from_str(&val) {
                snap.fleet_and_social_ops
                    .wh_local_witnesses
                    .entry(rest.to_string())
                    .or_insert(set);
            }
        } else if let Some(id_str) = key.strip_prefix("structure_defense:") {
            if let (Ok(id), Ok(st)) = (id_str.parse::<i64>(), serde_json::from_str(&val)) {
                snap.cluster_and_runtime
                    .structure_defense
                    .entry(id)
                    .or_insert(st);
            }
        } else if let Some(id_str) = key.strip_prefix("alliance_sov_cfg:") {
            if let (Ok(id), Ok(cfg)) = (id_str.parse::<i64>(), serde_json::from_str(&val)) {
                snap.cluster_and_runtime
                    .alliance_sov_cfg
                    .entry(id)
                    .or_insert(cfg);
            }
        } else if let Some(rest) = key.strip_prefix("portrait:") {
            snap.cluster_and_runtime
                .portraits
                .entry(rest.to_string())
                .or_insert(val);
        } else if let Some(id_str) = key.strip_prefix("paperdoll_state:") {
            if let Ok(id) = id_str.parse::<i64>() {
                snap.cluster_and_runtime
                    .paperdoll_states
                    .entry(id)
                    .or_insert(val);
            }
        } else if let Some(id_str) = key.strip_prefix("char_system_visits:") {
            if let (Ok(id), Ok(raw_map)) = (
                id_str.parse::<i64>(),
                serde_json::from_str::<HashMap<u32, LegacyVisitRecord>>(&val),
            ) {
                let converted = raw_map
                    .into_iter()
                    .map(|(sys_id, rec)| {
                        let visit = match rec {
                            LegacyVisitRecord::Full(r) => r,
                            LegacyVisitRecord::LegacyCount(visits) => SolarSystemVisitRecord {
                                visits,
                                last_visited_ticks: 0,
                            },
                        };
                        (sys_id, visit)
                    })
                    .collect();
                snap.cluster_and_runtime
                    .char_system_visits
                    .entry(id)
                    .or_insert(converted);
            }
        } else if let Some(id_str) = key.strip_prefix("auto_reloaded_event:") {
            if let (Ok(id), Ok(ev)) = (id_str.parse::<i64>(), serde_json::from_str(&val)) {
                snap.cluster_and_runtime
                    .auto_reloaded_event
                    .entry(id)
                    .or_insert(ev);
            }
        } else if let Some(id_str) = key.strip_prefix("mct_slots:") {
            if let (Ok(id), Ok(slots)) = (id_str.parse::<u64>(), serde_json::from_str(&val)) {
                snap.cluster_and_runtime.mct_slots.entry(id).or_insert(slots);
            }
        } else if let Some(id_str) = key.strip_prefix("account_resculpt_tokens:") {
            if let Ok(cnt) = val.parse::<u32>() {
                snap.cluster_and_runtime
                    .account_resculpt_tokens
                    .entry(id_str.to_string())
                    .or_insert(cnt);
            }
        } else if let Some(id_str) = key.strip_prefix("resculpt_pending:") {
            if let Ok(id) = id_str.parse::<i64>() {
                if val == "true" || val == "1" {
                    snap.cluster_and_runtime.resculpt_pending.insert(id);
                }
            }
        } else if let Some(id_str) = key.strip_prefix("char_cohorts:") {
            if let (Ok(id), Ok(list)) = (id_str.parse::<i64>(), serde_json::from_str(&val)) {
                snap.cluster_and_runtime
                    .char_cohorts
                    .entry(id)
                    .or_insert(list);
            }
        } else if let Some(id_str) = key.strip_prefix("user_cohorts:") {
            if let (Ok(id), Ok(list)) = (id_str.parse::<u64>(), serde_json::from_str(&val)) {
                snap.cluster_and_runtime
                    .user_cohorts
                    .entry(id)
                    .or_insert(list);
            }
        } else if let Some(id_str) = key.strip_prefix("character_lock:") {
            if let Ok(id) = id_str.parse::<i64>() {
                snap.cluster_and_runtime
                    .character_locks
                    .entry(id)
                    .or_insert(val);
            }
        } else if let Some(id_str) = key.strip_prefix("omega_downgraded:") {
            if let Ok(id) = id_str.parse::<u64>() {
                if val == "true" || val == "1" {
                    snap.cluster_and_runtime.omega_downgraded.insert(id);
                }
            }
        }
        // character_medals:* / char_medals:* were redundant copies of snap.char_medals and are dropped.
    }

    // 4. Legacy per-entity & global `service_state_blobs` and `#945` sentinel `owner_notes` -> `entity_kv` (`#913`)
    migrate_legacy_entity_kv(snap);
}

fn migrate_kv_if_absent<T: serde::Serialize>(
    snap: &mut WorldStateSnapshot,
    ns: Ns,
    entity_id: i64,
    sub_key: &str,
    val: &T,
    updated_unix: u64,
) {
    if !snap.kv(ns).contains(entity_id, sub_key) {
        let _ = snap
            .kv_mut(ns)
            .set_with_unix(entity_id, sub_key, val, updated_unix);
    }
}

fn migrate_legacy_entity_kv(snap: &mut WorldStateSnapshot) {
    let default_unix = snap
        .service_state_blobs
        .get("__server_now_unix")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    // 4a. Sentinel `owner_notes` entries (`note_id < -10_000_099`, `label == "home_station_remote_cooldown"`, `#945`).
    let hs_keys: Vec<i64> = snap
        .owner_notes
        .iter()
        .filter(|(&nid, rec)| {
            nid < -10_000_099 && rec.label == "home_station_remote_cooldown"
        })
        .map(|(&nid, _)| nid)
        .collect();
    for nid in hs_keys {
        if let Some(rec) = snap.owner_notes.remove(&nid) {
            let char_id = if rec.character_id < 0 {
                -rec.character_id
            } else {
                -10_000_099 - nid
            };
            if char_id > 0 {
                if let Ok(next_time) = rec.note.parse::<u64>() {
                    migrate_kv_if_absent(
                        snap,
                        Ns::HomeStationRemoteCooldown,
                        char_id,
                        "",
                        &next_time,
                        rec.timestamp_unix,
                    );
                }
            }
        }
    }

    // 4b. Unbounded per-character / per-user JSON map blobs in `service_state_blobs`.
    if let Some(raw) = snap.service_state_blobs.remove("alpha_avail_notified") {
        if let Ok(map) = serde_json::from_str::<HashMap<i64, bool>>(&raw) {
            for (cid, notified) in map {
                if notified {
                    migrate_kv_if_absent(
                        snap,
                        Ns::AlphaAvailNotified,
                        cid,
                        "",
                        &true,
                        default_unix,
                    );
                }
            }
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("starter_plex_granted_users") {
        if let Ok(set) = serde_json::from_str::<std::collections::BTreeSet<u64>>(&raw) {
            for uid in set {
                migrate_kv_if_absent(
                    snap,
                    Ns::StarterPlexGranted,
                    uid as i64,
                    "",
                    &true,
                    default_unix,
                );
            }
        } else if let Ok(map) = serde_json::from_str::<HashMap<u64, bool>>(&raw) {
            for (uid, granted) in map {
                if granted {
                    migrate_kv_if_absent(
                        snap,
                        Ns::StarterPlexGranted,
                        uid as i64,
                        "",
                        &true,
                        default_unix,
                    );
                }
            }
        }
    }

    // 4c. Global single-key entries in `service_state_blobs` -> `entity_kv` (`entity_id = 0`).
    if let Some(raw) = snap.service_state_blobs.remove("community_track_qa_mode") {
        let b = matches!(raw.trim(), "1" | "true" | "True");
        migrate_kv_if_absent(snap, Ns::CommunityTrackQaMode, 0, "", &b, default_unix);
    }
    if let Some(raw) = snap.service_state_blobs.remove("zarzakh_horizon_siege_active") {
        let b = matches!(raw.trim(), "1" | "true" | "True");
        migrate_kv_if_absent(snap, Ns::ZarzakhHorizonSiegeActive, 0, "", &b, default_unix);
    }
    if let Some(raw) = snap
        .service_state_blobs
        .remove("industry_system_cost_indices_daily")
    {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
            migrate_kv_if_absent(
                snap,
                Ns::IndustrySystemCostIndicesDaily,
                0,
                "",
                &val,
                default_unix,
            );
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("dynamic_resource_settings") {
        if let Ok(val) = serde_json::from_str::<HashMap<String, serde_json::Value>>(&raw) {
            migrate_kv_if_absent(snap, Ns::DynamicResourceSettings, 0, "", &val, default_unix);
        }
    }
    if let Some(raw) = snap.service_state_blobs.remove("market_seeder_version") {
        migrate_kv_if_absent(snap, Ns::MarketSeederVersion, 0, "", &raw, default_unix);
    }
    if let Some(raw) = snap.service_state_blobs.remove("xmpp:hostname") {
        migrate_kv_if_absent(snap, Ns::XmppHostname, 0, "", &raw, default_unix);
    }
    if let Some(raw) = snap
        .service_state_blobs
        .remove("kiring:eveguard_server_address")
    {
        migrate_kv_if_absent(
            snap,
            Ns::KiringConfig,
            0,
            "eveguard_server_address",
            &raw,
            default_unix,
        );
    }
    if let Some(raw) = snap.service_state_blobs.remove("bannedwords:words") {
        migrate_kv_if_absent(snap, Ns::BannedWords, 0, "words", &raw, default_unix);
    }
    if let Some(raw) = snap.service_state_blobs.remove("bannedwords:regexes") {
        migrate_kv_if_absent(snap, Ns::BannedWords, 0, "regexes", &raw, default_unix);
    }

    // 4d. Prefixed per-entity and sub-key entries in `service_state_blobs` -> `entity_kv`.
    let kv_keys: Vec<String> = snap
        .service_state_blobs
        .keys()
        .filter(|k| {
            k.starts_with("omega_weekly_sp_claim:")
                || k.starts_with("contract_orig_flag:")
                || k.starts_with("contract_bid_div:")
                || k.starts_with("char_transfer_consent_token:")
                || k.starts_with("char_transfer_last_unix:")
                || k.starts_with("char_transfer_evict_session:")
                || k.starts_with("xmpp:token:")
                || k.starts_with("xmpp:help_channel:")
                || k.starts_with("cache_setting:")
        })
        .cloned()
        .collect();

    for key in kv_keys {
        let Some(val) = snap.service_state_blobs.remove(&key) else {
            continue;
        };
        if let Some(uid_str) = key.strip_prefix("omega_weekly_sp_claim:") {
            if let (Ok(uid), Ok(ts)) = (uid_str.parse::<u64>(), val.parse::<u64>()) {
                migrate_kv_if_absent(
                    snap,
                    Ns::OmegaWeeklySpClaim,
                    uid as i64,
                    "",
                    &ts,
                    default_unix,
                );
            }
        } else if let Some(rest) = key.strip_prefix("contract_orig_flag:") {
            if let Some((cid_str, iid_str)) = rest.split_once(':') {
                if let (Ok(cid), Ok(flag_u16)) = (cid_str.parse::<i64>(), val.parse::<u16>()) {
                    migrate_kv_if_absent(
                        snap,
                        Ns::ContractOrigFlag,
                        cid,
                        iid_str,
                        &flag_u16,
                        default_unix,
                    );
                }
            }
        } else if let Some(rest) = key.strip_prefix("contract_bid_div:") {
            if let Some((cid_str, bidder_str)) = rest.split_once(':') {
                if let (Ok(cid), Ok(div_u8)) = (cid_str.parse::<i64>(), val.parse::<u8>()) {
                    migrate_kv_if_absent(
                        snap,
                        Ns::ContractBidDiv,
                        cid,
                        bidder_str,
                        &div_u8,
                        default_unix,
                    );
                }
            }
        } else if let Some(uid_str) = key.strip_prefix("char_transfer_consent_token:") {
            if let Ok(uid) = uid_str.parse::<u64>() {
                migrate_kv_if_absent(
                    snap,
                    Ns::CharTransferConsentToken,
                    uid as i64,
                    "",
                    &val,
                    default_unix,
                );
            }
        } else if let Some(cid_str) = key.strip_prefix("char_transfer_last_unix:") {
            if let (Ok(cid), Ok(ts)) = (cid_str.parse::<i64>(), val.parse::<u64>()) {
                migrate_kv_if_absent(
                    snap,
                    Ns::CharTransferLastUnix,
                    cid,
                    "",
                    &ts,
                    default_unix,
                );
            }
        } else if let Some(cid_str) = key.strip_prefix("char_transfer_evict_session:") {
            if let (Ok(cid), Ok(ts)) = (cid_str.parse::<i64>(), val.parse::<u64>()) {
                migrate_kv_if_absent(
                    snap,
                    Ns::CharTransferEvictSession,
                    cid,
                    "",
                    &ts,
                    default_unix,
                );
            }
        } else if let Some(cid_str) = key.strip_prefix("xmpp:token:") {
            if let Ok(cid) = cid_str.parse::<i64>() {
                migrate_kv_if_absent(snap, Ns::XmppAuthToken, cid, "", &val, default_unix);
            }
        } else if let Some(lang) = key.strip_prefix("xmpp:help_channel:") {
            migrate_kv_if_absent(snap, Ns::XmppHelpChannel, 0, lang, &val, default_unix);
        } else if let Some(sub) = key.strip_prefix("cache_setting:") {
            migrate_kv_if_absent(snap, Ns::CacheSetting, 0, sub, &val, default_unix);
        }
    }
}
