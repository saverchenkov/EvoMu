//! `pve_exploration` — PvE Cosmic Combat Anomalies, Combat Signatures, DED Rated Complexes,
//! Escalations & Expeditions, and Wormhole Sleeper Combat Sites (`Phase 8: W-3 & W-4`).
//!
// parity: feat:pve.combat-anomalies, feat:pve.combat-signatures, feat:pve.ded-complexes
// parity: feat:pve.escalations, feat:pve.expeditions, feat:pve.wormhole-sites

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_hacking_sites::handle_pve_hacking;
pub use eve_db::{
    is_lifetime_expired, DungeonInstanceId, DungeonTemplateId, EscalationInstanceId,
    ExplorationSiteId,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SolarSystemId, TypeId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub type CharacterId = i64;
pub type ItemId = i64;

/// Canonical string identifier for a persisted `PveSiteInstance` (`"PVE-ANOM-..."`, `"DED-..."`,
/// `"EXP-..."`, etc.). Distinct from `DungeonTemplateId` and `DungeonInstanceId`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExplorationSiteKey(pub String);

/// Borrowed canonical `PveSiteInstance` string key (`&str`) preventing accidental confusion with
/// numeric/stringified `DungeonTemplateId`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExplorationSiteKeyRef<'a>(pub &'a str);

impl<'a> ExplorationSiteKeyRef<'a> {
    pub const fn as_str(self) -> &'a str {
        self.0
    }
}

mod tuple_map_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<S, K1, K2, V>(
        map: &BTreeMap<(K1, K2), V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        K1: Serialize + Copy,
        K2: Serialize + Copy,
        V: Serialize,
    {
        let entries: Vec<((K1, K2), &V)> = map.iter().map(|(&(k1, k2), v)| ((k1, k2), v)).collect();
        entries.serialize(serializer)
    }

    pub fn deserialize<'de, D, K1, K2, V>(deserializer: D) -> Result<BTreeMap<(K1, K2), V>, D::Error>
    where
        D: Deserializer<'de>,
        K1: Deserialize<'de> + Ord,
        K2: Deserialize<'de> + Ord,
        V: Deserialize<'de>,
    {
        let entries: Vec<((K1, K2), V)> = Vec::deserialize(deserializer)?;
        Ok(entries.into_iter().collect())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PveSiteInstance {
    pub site_id: String,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub solar_system_id: SolarSystemId,
    pub site_category: String,
    pub ded_rating: Option<u8>,
    pub wormhole_class: Option<u8>,
    pub position: [f64; 3],
    pub probe_required: bool,
    pub signal_strength: f64,
    pub current_room: u8,
    pub total_rooms: u8,
    pub current_wave: u8,
    pub total_waves: u8,
    pub active_npc_ball_ids: Vec<i64>,
    pub hackable_can_ids: Vec<ItemId>,
    pub npc_guards_spawned: bool,
    pub ghost_timer_started_at: Option<u64>,
    pub ghost_detonate_after_sec: u64,
    pub ghost_detonated: bool,
    pub sleeper_rift_stabilized: bool,
    pub sleeper_sentry_disabled: bool,
    pub sleeper_gas_cloud_active: bool,
    pub cleared: bool,
    #[serde(default)]
    pub created_unix: u64,
    #[serde(default)]
    pub lifetime_seconds: u64,
    #[serde(default)]
    pub expedition_instance_id: Option<i64>,
    /// In-space acceleration gate ball for DED complexes (`keeper.ActivateAccelerationGate(gate_item_id)`).
    #[serde(default)]
    pub gate_ball_id: Option<i64>,
    #[serde(default)]
    pub activated_at_downtime: Option<u64>,
    #[serde(default)]
    pub dread_escalation_triggers: u8,
    #[serde(default)]
    pub carrier_escalation_triggers: u8,
    #[serde(default)]
    pub active_escalation_capitals: Vec<crate::services::wormhole_pve_escalations::ActiveCapitalEscalationEntry>,
    #[serde(default)]
    pub is_shattered_resource_site: bool,
    #[serde(default)]
    pub delayed_wave_triggered: bool,
    #[serde(default)]
    pub relay_spawned: bool,
}

impl PveSiteInstance {
    pub fn canonical_key(&self) -> ExplorationSiteKeyRef<'_> {
        ExplorationSiteKeyRef(&self.site_id)
    }

    pub const fn template_id(&self) -> DungeonTemplateId {
        DungeonTemplateId::new(self.dungeon_id)
    }
}

pub use crate::services::pve_expeditions::EscalatingPathEntry;
pub use crate::services::pve_sleeper_caches::{
    sleeper_cache_ship_restrictions, sleeper_cache_tier_for_dungeon,
    validate_sleeper_cache_passage, SleeperCacheTier,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HackingNodeKind {
    Empty,
    SystemCore,
    Firewall,
    Antivirus,
    RestorationNode,
    VirusSuppressor,
    UtilitySubsystem,
    DataCache,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HackingUtilityKind {
    SelfRepair = 1,
    KernelRot = 2,
    PolymorphicShield = 3,
    SecondaryVector = 4,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HackingTileState {
    pub tile_index: u32,
    pub revealed: bool,
    pub blocked_by_defensive: bool,
    pub kind: HackingNodeKind,
    pub coherence: i32,
    pub strength: i32,
    pub utility_drop: Option<HackingUtilityKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HackingGameSession {
    pub game_id: i64,
    pub character_id: CharacterId,
    pub solar_system_id: SolarSystemId,
    pub container_id: ItemId,
    pub site_id: String,
    pub is_relic: bool,
    pub is_ghost_site: bool,
    pub is_sleeper_cache_puzzle: bool,
    pub virus_coherence: i32,
    pub max_virus_coherence: i32,
    pub base_virus_strength: i32,
    pub polymorphic_shield_charges: u8,
    pub secondary_vector_target: Option<(u32, u8)>,
    pub utilities: Vec<Option<HackingUtilityKind>>,
    pub tiles: Vec<HackingTileState>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpewedLootCan {
    pub can_item_id: ItemId,
    pub parent_container_id: ItemId,
    pub solar_system_id: SolarSystemId,
    pub position: [f64; 3],
    pub loot_type_id: TypeId,
    pub loot_quantity: i64,
    pub spawned_at_sec: u64,
    pub despawn_at_sec: u64,
    pub scooped: bool,
}

#[derive(Default, Serialize, Deserialize)]
pub struct PveExplorationRegistry {
    #[serde(skip)]
    pub db_weak: std::sync::Weak<eve_db::WorldRepository>,
    #[serde(skip)]
    pub current_now_sec: u64,
    pub seeded_systems: BTreeMap<SolarSystemId, bool>,
    pub sites_by_system: BTreeMap<SolarSystemId, Vec<PveSiteInstance>>,
    pub escalations_by_char: BTreeMap<CharacterId, Vec<EscalatingPathEntry>>,
    #[serde(default, with = "tuple_map_serde")]
    pub hacking_games: BTreeMap<(CharacterId, ItemId), HackingGameSession>,
    pub spewed_cans: BTreeMap<ItemId, SpewedLootCan>,
    #[serde(default)]
    pub hacked_containers: BTreeSet<(SolarSystemId, ItemId)>,
    #[serde(default, with = "tuple_map_serde")]
    pub failure_counts: BTreeMap<(SolarSystemId, ItemId), u8>,
    #[serde(default, with = "tuple_map_serde")]
    pub self_repair_turns: BTreeMap<(CharacterId, ItemId), u8>,
    #[serde(default, with = "tuple_map_serde")]
    pub structure_char_failures: BTreeMap<(CharacterId, ItemId), u8>,
    pub sim_clock_sec: u64,
    pub next_id_seq: i64,
    #[serde(default)]
    pub downtime_counter: u64,
    #[serde(default)]
    pub active_relays: BTreeMap<i64, crate::services::wormhole_pve_escalations::DecloakedRelayInfo>,
    #[serde(default)]
    pub sentry_last_fire_unix: BTreeMap<i64, f64>,
    #[serde(default)]
    pub sentry_last_fire_sim_s: BTreeMap<i64, f64>,
    /// Persistent monotonic entity IDs keyed by canonical `site_id` (`snap.alloc_entity_id()`, `PVE-213`).
    #[serde(default)]
    pub site_instance_ids: BTreeMap<String, ExplorationSiteId>,
}

impl PveExplorationRegistry {
    /// Returns the persisted typed [`ExplorationSiteId`] for `site`, falling back only for
    /// legacy pre-migration snapshots that predate `site_instance_ids`.
    pub fn exploration_site_id(&self, site: &PveSiteInstance) -> ExplorationSiteId {
        self.site_instance_ids.get(&site.site_id).copied().unwrap_or_else(|| {
            ExplorationSiteId::new(crate::services::pve_dungeon_cache::registry_instance_id(
                &site.site_id,
            ))
        })
    }

    /// Records the allocated [`ExplorationSiteId`] for canonical `site_id`.
    pub fn assign_site_instance_id(&mut self, site_id: &str, id: ExplorationSiteId) {
        self.site_instance_ids.insert(site_id.to_string(), id);
    }
}
pub type HackingSiteRegistry = PveExplorationRegistry;
pub const PVE_EXPLORATION_NOTE_ID: i64 = -8_200_000_000_000;

pub fn db_pve_key(db: &std::sync::Arc<eve_db::WorldRepository>) -> usize {
    std::sync::Arc::as_ptr(db) as usize
}

pub fn ctx_pve_key(ctx: &ServerContext) -> usize {
    db_pve_key(&ctx.db)
}

fn parse_pve_exploration_reg_from_note(
    note: Option<&eve_db::OwnerNoteRecord>,
    db: &std::sync::Arc<eve_db::WorldRepository>,
    now_unix: u64,
) -> (PveExplorationRegistry, String) {
    let (mut reg, orig_json) = match note {
        Some(n) => match serde_json::from_str::<PveExplorationRegistry>(&n.note) {
            Ok(parsed) => (parsed, n.note.clone()),
            Err(_) => (PveExplorationRegistry::default(), String::new()),
        },
        None => (PveExplorationRegistry::default(), String::new()),
    };
    reg.db_weak = std::sync::Arc::downgrade(db);
    reg.current_now_sec = now_unix;
    (reg, orig_json)
}

fn build_pve_exploration_note_if_changed(
    reg: &PveExplorationRegistry,
    orig_json: &str,
) -> Option<eve_db::OwnerNoteRecord> {
    let new_json = serde_json::to_string(reg).ok()?;
    if new_json == orig_json {
        return None;
    }
    Some(eve_db::OwnerNoteRecord {
        note_id: PVE_EXPLORATION_NOTE_ID,
        character_id: -82,
        item_id: 0,
        label: "pve_exploration_registry".into(),
        note: new_json,
        timestamp_unix: reg.sim_clock_sec,
    })
}

pub fn with_pve_exploration_reg_db<R>(
    db: &std::sync::Arc<eve_db::WorldRepository>,
    now_unix: u64,
    f: impl FnOnce(&mut PveExplorationRegistry) -> R,
) -> R {
    db.with_mut_snapshot(|snap| {
        let (mut reg, orig_json) = parse_pve_exploration_reg_from_note(
            snap.owner_notes.get(&PVE_EXPLORATION_NOTE_ID),
            db,
            now_unix,
        );
        let out = f(&mut reg);
        if let Some(updated_note) = build_pve_exploration_note_if_changed(&reg, &orig_json) {
            snap.owner_notes.insert(PVE_EXPLORATION_NOTE_ID, updated_note);
        }
        out
    })
}

pub fn with_pve_exploration_reg_snap_db<R>(
    db: &std::sync::Arc<eve_db::WorldRepository>,
    now_unix: u64,
    f: impl FnOnce(&mut PveExplorationRegistry, &mut eve_db::WorldStateSnapshot) -> R,
) -> R {
    db.with_mut_snapshot(|snap| {
        let (mut reg, orig_json) = parse_pve_exploration_reg_from_note(
            snap.owner_notes.get(&PVE_EXPLORATION_NOTE_ID),
            db,
            now_unix,
        );
        let out = f(&mut reg, snap);
        if let Some(updated_note) = build_pve_exploration_note_if_changed(&reg, &orig_json) {
            snap.owner_notes.insert(PVE_EXPLORATION_NOTE_ID, updated_note);
        }
        out
    })
}

pub fn try_with_pve_exploration_reg_db<R, E>(
    db: &std::sync::Arc<eve_db::WorldRepository>,
    now_unix: u64,
    f: impl FnOnce(&mut PveExplorationRegistry) -> Result<R, E>,
) -> Result<R, E>
where
    E: eve_db::FromWalError,
{
    db.try_with_mut_snapshot(|snap| {
        let (mut reg, orig_json) = parse_pve_exploration_reg_from_note(
            snap.owner_notes.get(&PVE_EXPLORATION_NOTE_ID),
            db,
            now_unix,
        );
        let out = f(&mut reg)?;
        if let Some(updated_note) = build_pve_exploration_note_if_changed(&reg, &orig_json) {
            snap.owner_notes.insert(PVE_EXPLORATION_NOTE_ID, updated_note);
        }
        Ok(out)
    })
}

pub fn try_with_pve_exploration_reg_snap<R, E>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut PveExplorationRegistry, &mut eve_db::WorldStateSnapshot) -> Result<R, E>,
) -> Result<R, E>
where
    E: eve_db::FromWalError,
{
    let now_unix = ctx.now_unix();
    ctx.db.try_with_mut_snapshot(|snap| {
        let (mut reg, orig_json) = parse_pve_exploration_reg_from_note(
            snap.owner_notes.get(&PVE_EXPLORATION_NOTE_ID),
            &ctx.db,
            now_unix,
        );
        let out = f(&mut reg, snap)?;
        if let Some(updated_note) = build_pve_exploration_note_if_changed(&reg, &orig_json) {
            snap.owner_notes.insert(PVE_EXPLORATION_NOTE_ID, updated_note);
        }
        Ok(out)
    })
}

/// Reads `PveExplorationRegistry` cleanly from a DB snapshot without ever mutating `owner_notes`.
pub fn read_pve_exploration_reg_db<R>(
    db: &std::sync::Arc<eve_db::WorldRepository>,
    now_unix: u64,
    f: impl FnOnce(&PveExplorationRegistry) -> R,
) -> R {
    let (reg, _) = db.with_snapshot(|snap| {
        parse_pve_exploration_reg_from_note(
            snap.owner_notes.get(&PVE_EXPLORATION_NOTE_ID),
            db,
            now_unix,
        )
    });
    f(&reg)
}

pub fn read_pve_exploration_reg<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&PveExplorationRegistry) -> R,
) -> R {
    read_pve_exploration_reg_db(&ctx.db, ctx.now_unix(), f)
}

pub fn with_pve_exploration_reg<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut PveExplorationRegistry) -> R,
) -> R {
    with_pve_exploration_reg_db(&ctx.db, ctx.now_unix(), f)
}

pub fn try_with_pve_exploration_reg<R, E>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut PveExplorationRegistry) -> Result<R, E>,
) -> Result<R, E>
where
    E: eve_db::FromWalError,
{
    try_with_pve_exploration_reg_db(&ctx.db, ctx.now_unix(), f)
}

/// Single canonical expiration rule for `PveSiteInstance`: checks both its own finite positive
/// lifetime (`is_lifetime_expired`) and any linked escalation expiration in `reg`.
pub(crate) fn is_site_expired_in_reg(
    reg: &PveExplorationRegistry,
    site: &PveSiteInstance,
    now: u64,
) -> bool {
    is_lifetime_expired(site.created_unix, site.lifetime_seconds, now)
        || site.expedition_instance_id.is_some_and(|inst_id| {
            crate::services::pve_expeditions::linked_escalation_is_expired(reg, inst_id, now)
        })
}

pub(crate) struct ExpiredSitePruneResult {
    pub(crate) site_id: String,
    pub(crate) solar_system_id: SolarSystemId,
    pub(crate) drained_balls: Vec<(SolarSystemId, Vec<i64>)>,
    pub(crate) escalation_cleanup: crate::services::pve_expeditions::PrunedEscalationsCleanup,
}

/// Marks an expired site cleared, drains its NPC balls, and prunes any expired linked escalation.
pub(crate) fn expire_and_prune_site_in_reg(
    reg: &mut PveExplorationRegistry,
    solar_system_id: SolarSystemId,
    target_site_id: &str,
    now: u64,
) -> Option<ExpiredSitePruneResult> {
    let (exp_inst_id, drained_balls) = {
        let sites = reg.sites_by_system.get_mut(&solar_system_id)?;
        let site = find_site_by_canonical_id_mut(sites, target_site_id)?;
        let exp_inst_id = site.expedition_instance_id;
        site.cleared = true;
        let balls: Vec<i64> = site.active_npc_ball_ids.drain(..).collect();
        let drained = if balls.is_empty() {
            Vec::new()
        } else {
            vec![(solar_system_id, balls)]
        };
        (exp_inst_id, drained)
    };
    let escalation_cleanup = exp_inst_id
        .map(|inst_id| {
            crate::services::pve_expeditions::prune_expired_escalation_for_instance_in_reg(
                reg, inst_id, now,
            )
        })
        .unwrap_or_default();
    Some(ExpiredSitePruneResult {
        site_id: target_site_id.to_string(),
        solar_system_id,
        drained_balls,
        escalation_cleanup,
    })
}

fn apply_expired_site_prune_in_db(
    db: &eve_db::WorldRepository,
    pruned: ExpiredSitePruneResult,
) -> (
    Vec<(
        crate::services::pve_expeditions::CharacterId,
        Vec<EscalatingPathEntry>,
    )>,
    Vec<(SolarSystemId, Vec<i64>)>,
) {
    let expired_chars: Vec<(
        crate::services::pve_expeditions::CharacterId,
        Vec<EscalatingPathEntry>,
    )> = pruned
        .escalation_cleanup
        .expired_by_char
        .iter()
        .map(|(cid, entries, _)| (*cid, entries.clone()))
        .collect();
    db.with_mut_snapshot(|snap| {
        if let Some(anoms) = snap.cosmic_anomalies.get_mut(&pruned.solar_system_id) {
            for a in anoms
                .iter_mut()
                .filter(|a| a.anomaly_id == pruned.site_id && !a.cleared)
            {
                a.cleared = true;
            }
        }
        if let Some(sigs) = snap.cosmic_signatures.get_mut(&pruned.solar_system_id) {
            for s in sigs
                .iter_mut()
                .filter(|s| s.signature_id == pruned.site_id && !s.collapsed)
            {
                s.collapsed = true;
            }
        }
    });
    let combined = crate::services::pve_expeditions::merge_despawn_ball_batches(
        pruned.escalation_cleanup.site_npc_batches,
        pruned.drained_balls,
    );
    let removed_balls = crate::services::pve_expeditions::despawn_expired_escalations_in_db(
        db,
        &pruned.escalation_cleanup.expired_by_char,
        combined,
    );
    (expired_chars, removed_balls)
}

pub(crate) async fn finalize_expired_site_prune_core(
    db: &std::sync::Arc<eve_db::WorldRepository>,
    systems: &std::sync::Arc<crate::system_actor::SystemRegistry>,
    pruned: ExpiredSitePruneResult,
) -> Vec<(
    crate::services::pve_expeditions::CharacterId,
    Vec<EscalatingPathEntry>,
)> {
    let (expired_chars, removed_balls) = apply_expired_site_prune_in_db(db, pruned);
    if !removed_balls.is_empty() {
        crate::services::pve_expeditions::despawn_escalation_balls_in_loaded_systems(
            systems,
            removed_balls,
        )
        .await;
    }
    expired_chars
}

pub(crate) fn finalize_expired_site_prune_sync(
    ctx: &ServerContext,
    pruned: ExpiredSitePruneResult,
) -> Vec<(
    crate::services::pve_expeditions::CharacterId,
    Vec<EscalatingPathEntry>,
)> {
    let (expired_chars, removed_balls) = apply_expired_site_prune_in_db(&ctx.db, pruned);
    if !removed_balls.is_empty() {
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let systems = std::sync::Arc::clone(&ctx.systems);
            rt.spawn(
                crate::services::pve_expeditions::despawn_escalation_balls_in_loaded_systems(
                    &systems,
                    removed_balls,
                ),
            );
        }
    }
    expired_chars
}

/// Persists expired-site pruning, despawns live balls, emits escalation change notices, and returns `SiteExpired`.
pub(crate) async fn reject_expired_site_prune(
    ctx: &ServerContext,
    pruned: ExpiredSitePruneResult,
) -> BridgeRpcError {
    let expired_sid = pruned.site_id.clone();
    let expired_chars = finalize_expired_site_prune_core(&ctx.db, &ctx.systems, pruned).await;
    crate::services::pve_expeditions::emit_pruned_escalation_notifications(ctx, &expired_chars);
    BridgeRpcError {
        code: "SiteExpired".into(),
        message: format!("PvE site {expired_sid} has expired"),
        client_error: None,
    }
}

/// Looks up an active (`!cleared && !is_site_expired_in_reg`) site strictly by canonical
/// `ExplorationSiteKeyRef` (`s.site_id`). Never falls back to expired/cleared sites or matching
/// `dungeon_id` (`#1495`).
pub(crate) fn find_site_by_canonical_key_in_reg<'a>(
    reg: &PveExplorationRegistry,
    sites: &'a [PveSiteInstance],
    key: ExplorationSiteKeyRef<'_>,
    now: u64,
) -> Option<&'a PveSiteInstance> {
    sites
        .iter()
        .find(|s| !s.cleared && !is_site_expired_in_reg(reg, s, now) && s.canonical_key() == key)
}

/// Read-only / lifecycle metadata lookup preferring active -> uncleared -> cleared sites strictly
/// by canonical `ExplorationSiteKeyRef` (`s.site_id`). Never falls back to `dungeon_id` (`#1495`).
pub(crate) fn find_site_by_canonical_key_any_state_in_reg<'a>(
    reg: &PveExplorationRegistry,
    sites: &'a [PveSiteInstance],
    key: ExplorationSiteKeyRef<'_>,
    now: u64,
) -> Option<&'a PveSiteInstance> {
    find_site_by_canonical_key_in_reg(reg, sites, key, now)
        .or_else(|| sites.iter().find(|s| !s.cleared && s.canonical_key() == key))
        .or_else(|| sites.iter().find(|s| s.canonical_key() == key))
}

/// Looks up an active (uncleared, non-expired) exploration site strictly by `ExplorationSiteId`.
#[cfg(test)]
pub(crate) fn find_active_site_by_id_in_reg<'a>(
    reg: &PveExplorationRegistry,
    sites: &'a [PveSiteInstance],
    site_id: ExplorationSiteId,
    now: u64,
) -> Option<&'a PveSiteInstance> {
    sites.iter().find(|s| {
        !s.cleared && !is_site_expired_in_reg(reg, s, now) && reg.exploration_site_id(s) == site_id
    })
}

/// Looks up an active (uncleared, non-expired) exploration site strictly by `DungeonTemplateId`.
#[cfg(test)]
pub(crate) fn find_active_site_by_dungeon_template_in_reg<'a>(
    reg: &PveExplorationRegistry,
    sites: &'a [PveSiteInstance],
    dungeon_id: DungeonTemplateId,
    now: u64,
) -> Option<&'a PveSiteInstance> {
    sites.iter().find(|s| {
        !s.cleared && !is_site_expired_in_reg(reg, s, now) && s.template_id() == dungeon_id
    })
}

fn find_site_index_by_canonical_id(sites: &[PveSiteInstance], site_id: &str) -> Option<usize> {
    sites
        .iter()
        .position(|s| !s.cleared && s.site_id == site_id)
        .or_else(|| sites.iter().position(|s| s.site_id == site_id))
}

/// Matches strictly by canonical `site_id`, preferring uncleared entries over cleared ones.
pub(crate) fn find_site_by_canonical_id<'a>(
    sites: &'a [PveSiteInstance],
    site_id: &str,
) -> Option<&'a PveSiteInstance> {
    find_site_index_by_canonical_id(sites, site_id).map(|idx| &sites[idx])
}

/// Matches strictly by canonical `site_id`, preferring uncleared entries over cleared ones.
pub(crate) fn find_site_by_canonical_id_mut<'a>(
    sites: &'a mut [PveSiteInstance],
    site_id: &str,
) -> Option<&'a mut PveSiteInstance> {
    find_site_index_by_canonical_id(sites, site_id).map(|idx| &mut sites[idx])
}

pub fn next_pve_seq(reg: &mut PveExplorationRegistry) -> i64 {
    reg.next_id_seq += 1;
    reg.next_id_seq
}

pub fn now_sec(reg: &PveExplorationRegistry) -> u64 {
    let base = if reg.current_now_sec > 0 {
        reg.current_now_sec
    } else {
        1_775_000_000
    };
    base.max(reg.sim_clock_sec)
}

pub fn advance_pve_exploration_clock(ctx: &ServerContext, delta_sec: u64) {
    let cur_now = ctx.now_unix();
    with_pve_exploration_reg(ctx, |reg| {
        if reg.sim_clock_sec < cur_now {
            reg.sim_clock_sec = cur_now;
        }
        reg.sim_clock_sec += delta_sec;
    });
}

// parity: feat:pve.wormhole-sites
pub fn wormhole_class_for_system(solar_system_id: SolarSystemId) -> Option<u8> {
    let wh_c = crate::services::wormhole_lifecycle_ops::system_wormhole_profile(solar_system_id).0;
    match wh_c {
        1..=6 | 12..=18 => Some(wh_c),
        _ => None,
    }
}

pub use crate::services::pve_exploration_seeding::{
    anomaly_dungeons_for_system, build_wave_npc_balls, build_wave_npc_balls_in_snap,
    ded_complex_for_rating, ensure_system_pve_sites_seeded, pirate_faction_for_system,
    PirateFaction,
};
pub use crate::services::pve_exploration_sites::{
    advance_site_wave_or_complete, ded_ship_restriction_for_rating,
    handle_pve_site_npc_destruction, handle_pve_site_npc_destruction_core,
    notify_pve_destruction_events, pve_site_category_and_dungeon_id,
};
pub use crate::services::wormhole_pve_escalations::{
    drop_site_loot_into_space, drop_site_loot_into_space_core,
};


fn resolve_session_system_id(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<SolarSystemId, BridgeRpcError> {
    session
        .solar_system_id
        .or_else(|| {
            session.character_id.and_then(|cid| {
                ctx.db
                    .with_snapshot(|s| s.characters.get(&cid).map(|c| c.solar_system_id))
            })
        })
        .ok_or_else(|| BridgeRpcError {
            code: "NotInSpace".into(),
            message: "Action requires an active solar system session".into(),
            client_error: None,
        })
}

fn rpc_msg_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    let msg = message.into();
    BridgeRpcError::with_client_error(
        code,
        msg.clone(),
        eve_proto::ClientError::CustomInfo(msg),
    )
}

fn resolve_exploration_space_context(
    ctx: &ServerContext,
    session: &SessionState,
    require_ship: bool,
) -> Result<(i64, SolarSystemId), BridgeRpcError> {
    let res = if require_ship {
        ctx.require_undocked_space_ship(
            session,
            "NotInSpace",
            "Cannot access dungeon site while docked",
            false,
            false,
        )
        .map(|(cid, sys_id, _)| (cid, sys_id))
    } else {
        ctx.require_undocked_space_system(
            session,
            "NotInSpace",
            "Cannot access dungeon site while docked",
            false,
            false,
        )
    };
    match res {
        Ok(v) => Ok(v),
        Err(e) if e.code == "NoSolarSystem" => {
            if ctx.is_session_docked_strict_policy(session, false) {
                return Err(rpc_msg_err(
                    "NotInSpace",
                    "Cannot access dungeon site while docked",
                ));
            }
            let (_, char_id) = ServerContext::require_character(session)?;
            let sys_id = resolve_session_system_id(ctx, session)?;
            if require_ship && session.ship_id.filter(|&id| id > 0).is_none() {
                return Err(rpc_msg_err("NoActiveShip", "Session has no active ship"));
            }
            Ok((char_id, sys_id))
        }
        Err(e) => Err(e),
    }
}

fn naturally_seedable_incursion_dungeon_id(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    site_id: &str,
) -> Option<u32> {
    let const_id = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.constellation_id)?;
    let eligible = crate::services::pve_incursion_lifecycle::eligible_incursion_constellations(ctx);
    if !eligible.into_iter().take(3).any(|(cid, _)| cid == const_id) {
        return None;
    }
    let mut sys_list: Vec<SolarSystemId> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.constellation_id == const_id)
        .map(|s| s.solar_system_id)
        .collect();
    sys_list.sort_unstable();
    let (stg, vg, as_s, hq) =
        crate::services::pve_incursion_lifecycle::assign_constellation_system_roles(
            &ctx.sde,
            const_id,
            &sys_list,
        );
    let vg1 = vg[0];
    let vg2 = vg.get(1).copied().unwrap_or(vg1);
    let as1 = as_s[0];
    let as2 = as_s.get(1).copied().unwrap_or(as1);
    [
        (stg, 5164_u32),
        (vg1, 5165),
        (as1, 5168),
        (hq, 5175),
        (hq, 5158),
        (vg2, 5166),
        (as2, 5167),
        (hq, 5171),
    ]
    .into_iter()
    .find_map(|(expected_sys, did)| {
        (expected_sys == sys_id
            && (site_id == format!("INC-{const_id}-{did}") || site_id == format!("INC-{did}")))
        .then_some(did)
    })
}

#[cfg(test)]
fn is_naturally_seedable_incursion_site(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    site_id: &str,
) -> bool {
    naturally_seedable_incursion_dungeon_id(ctx, sys_id, site_id).is_some_and(|did| did != 5158)
}

pub async fn handle_pve_exploration(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Some(res) = Box::pin(handle_pve_hacking(ctx, session, service, method, args, kwargs)).await {
        return Some(res);
    }

    match (service, method) {
        // parity: feat:pve.combat-anomalies, feat:pve.wormhole-sites
        ("scanSvc", "GetCombatAnomalyInstances") => {
            Some(Box::pin(handle_get_combat_anomaly_instances(ctx, session, args)).await)
        }

        // parity: feat:pve.combat-anomalies, feat:pve.wormhole-sites, feat:pve.escalations, feat:pve.expeditions
        ("scanSvc", "EnterCombatAnomaly") => {
            Some(Box::pin(handle_enter_combat_anomaly(ctx, session, args)).await)
        }

        // parity: feat:pve.ded-complexes, feat:pve.sleeper-caches
        ("keeper", "GetDungeonShipRestrictions") | ("keeper", "CanEnterDungeonInCurrentShip") => {
            handle_dungeon_ship_restrictions(ctx, session, method, args)
        }

        // parity: feat:pve.ded-complexes, feat:pve.sleeper-caches
        ("keeper", "ActivateAccelerationGate")
        | ("beyonce", "ActivateAccelerationGate")
        | ("keeper", "EnterNextDungeonRoom") => {
            Box::pin(handle_activate_acceleration_gate(ctx, session, method, args)).await
        }

        // parity: feat:pve.sleeper-caches, feat:pve.ded-complexes
        ("keeper", "GetDungeonInfo") | ("keeper", "GetDungeonObjectivesData") => {
            Some(Box::pin(handle_get_dungeon_info(ctx, session, args)).await)
        }

        // parity: feat:pve.escalations, feat:pve.expeditions, rpc:dungeonExplorationMgr.GetMyEscalatingPathDetails
        ("dungeonExplorationMgr", "GetMyEscalatingPathDetails")
        | ("scanSvc", "GetMyEscalatingPathDetails")
        | ("dungeonExplorationMgr", "GetEscalations") => {
            Some(handle_get_escalating_path_details(ctx, session, args))
        }

        // parity: feat:pve.escalations, feat:pve.expeditions
        ("dungeonExplorationMgr", "GetEscalatingPathDungeonsEntered")
        | ("scanSvc", "GetEscalatingPathDungeonsEntered") => {
            Some(handle_get_escalating_path_dungeons_entered(ctx, session, service, args))
        }

        _ => None,
    }
}

/// Evaluates ship restrictions for `keeper.GetDungeonShipRestrictions` and
/// `keeper.CanEnterDungeonInCurrentShip` without mutating `EveDb` or seeding registries.
fn handle_dungeon_ship_restrictions(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let require_ship = method == "CanEnterDungeonInCurrentShip";
    let (_, sys_id) = match resolve_exploration_space_context(ctx, session, require_ship) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };

    if args.len() != 1 {
        let msg = format!("{method} requires a single dungeon or site identifier argument");
        return Some(Err(BridgeRpcError::with_client_error(
            "InvalidArguments",
            msg.clone(),
            eve_proto::ClientError::CustomInfo(msg),
        )));
    }
    let (arg0_int, arg0_str) = match args.first() {
        Some(PyRep::Integer(did)) => (Some(*did), None),
        Some(arg) => match arg.as_str() {
            Some(s) if !s.trim().is_empty() => (None, Some(s)),
            _ => {
                let msg = format!(
                    "{method} requires a positive integer dungeonID or non-empty siteID"
                );
                return Some(Err(BridgeRpcError::with_client_error(
                    "InvalidArguments",
                    msg.clone(),
                    eve_proto::ClientError::CustomInfo(msg),
                )));
            }
        },
        None => unreachable!(),
    };

    let mut sleeper_tier_opt: Option<(SleeperCacheTier, u32, &'static str)> = None;
    let mut ded_rating_opt: Option<u8> = None;
    let mut explicit_sde_dungeon: Option<(u32, String)> = None;

    if let Some(did) = arg0_int {
        if did <= 0 {
            let msg = format!("Invalid dungeon identifier {did}");
            return Some(Err(BridgeRpcError::with_client_error(
                "InvalidArguments",
                msg.clone(),
                eve_proto::ClientError::CustomInfo(msg),
            )));
        }
        if did > 10 {
            let tpl_opt = u32::try_from(did).ok().map(DungeonTemplateId::new);
            if let Some(tier) = tpl_opt.and_then(|t| sleeper_cache_tier_for_dungeon(t.as_u32())) {
                let name = match tier {
                    SleeperCacheTier::Limited => "Limited Sleeper Cache",
                    SleeperCacheTier::Standard => "Standard Sleeper Cache",
                    SleeperCacheTier::Superior => "Superior Sleeper Cache",
                };
                sleeper_tier_opt = Some((tier, did as u32, name));
            } else if let Some(d) = tpl_opt.and_then(|id| ctx.sde.dungeons.get(&id.as_u32())) {
                let dname = if d.name.trim().is_empty() {
                    format!("Dungeon {did}")
                } else {
                    d.name.trim().to_string()
                };
                explicit_sde_dungeon = Some((did as u32, dname));
            } else {
                let msg = format!("Dungeon {did} not found");
                return Some(Err(BridgeRpcError::with_client_error(
                    "DungeonNotFound",
                    msg.clone(),
                    eve_proto::ClientError::CustomInfo(msg),
                )));
            }
        } else {
            ded_rating_opt = Some(did.clamp(1, 10) as u8);
        }
    } else if let Some(sid) = arg0_str {
        let key_ref = ExplorationSiteKeyRef(sid);
        let matched_seeded = read_pve_exploration_reg(ctx, |reg| {
            let now = now_sec(reg);
            let Some(sites) = reg.sites_by_system.get(&sys_id) else {
                return false;
            };
            let Some(s) = find_site_by_canonical_key_any_state_in_reg(reg, sites, key_ref, now)
            else {
                return false;
            };
            if s.site_category == "SleeperCache" {
                if let Some(tier) = sleeper_cache_tier_for_dungeon(s.dungeon_id) {
                    sleeper_tier_opt = Some((tier, s.dungeon_id, "Sleeper Cache"));
                } else {
                    let dname = if !s.dungeon_name.trim().is_empty() {
                        s.dungeon_name.trim().to_string()
                    } else {
                        s.site_id.clone()
                    };
                    explicit_sde_dungeon = Some((s.dungeon_id, dname));
                }
            } else if let Some(r) = s.ded_rating {
                ded_rating_opt = Some(r);
            } else {
                let dname = if !s.dungeon_name.trim().is_empty() {
                    s.dungeon_name.trim().to_string()
                } else if let Some(d) = ctx
                    .sde
                    .dungeons
                    .get(&s.dungeon_id)
                    .filter(|d| !d.name.trim().is_empty())
                {
                    d.name.trim().to_string()
                } else {
                    s.site_id.clone()
                };
                explicit_sde_dungeon = Some((s.dungeon_id, dname));
            }
            true
        });
        if !matched_seeded {
            return Some(Err(rpc_msg_err(
                "DungeonNotFound",
                format!("Dungeon site {sid} not found"),
            )));
        }
    }

    let (did, dname, ded_rating_val, max_mass, label, allowed_groups) = if let Some((tier, d_id, name)) = sleeper_tier_opt {
        let (mass, lbl, grps) = sleeper_cache_ship_restrictions(tier);
        (d_id, name.to_string(), 0u8, mass, lbl, grps)
    } else if let Some((d_id, d_name)) = explicit_sde_dungeon {
        let rating = ded_rating_opt.unwrap_or(3);
        let (mass, lbl, grps) = ded_ship_restriction_for_rating(rating);
        (d_id, d_name, rating, mass, lbl, grps)
    } else {
        let rating = ded_rating_opt.unwrap_or(3);
        let (mass, lbl, grps) = ded_ship_restriction_for_rating(rating);
        let (d_id, d_name) = ded_complex_for_rating(&ctx.sde, sys_id, rating);
        (d_id, d_name.to_string(), rating, mass, lbl, grps)
    };

    let (ship_type_id, ship_group_id, ship_mass) = session
        .ship_id
        .and_then(|sid| {
            ctx.db.with_snapshot(|snap| {
                snap.items.get(&sid).map(|item| {
                    let t = ctx.sde.types.get(&item.type_id);
                    (item.type_id, t.map(|x| x.group_id).unwrap_or(25), t.map(|x| x.mass).unwrap_or(1_200_000.0))
                })
            })
        })
        .unwrap_or((587, 25, 1_067_000.0));

    let can_enter = allowed_groups.contains(&ship_group_id) && ship_mass <= max_mass;
    Some(Ok(PyRep::keyval([
        ("dungeonID", PyRep::Integer(did as i64)),
        ("dungeonName", PyRep::String(dname)),
        ("dedRating", PyRep::Integer(ded_rating_val as i64)),
        ("maxShipMass", PyRep::Real(max_mass)),
        ("restrictionDescription", PyRep::String(label.into())),
        ("allowedGroupIDs", PyRep::List(allowed_groups.iter().map(|&g| PyRep::Integer(g as i64)).collect())),
        ("shipTypeID", PyRep::Integer(ship_type_id as i64)),
        ("shipGroupID", PyRep::Integer(ship_group_id as i64)),
        ("canEnter", PyRep::Bool(can_enter)),
    ])))
}

#[allow(clippy::too_many_arguments)]
fn combat_anomaly_keyval(
    site_id: String,
    dungeon_id: u32,
    dungeon_name: String,
    site_category: String,
    wormhole_class: Option<u8>,
    current_wave: u8,
    total_waves: u8,
    active_npc_ball_ids: Vec<i64>,
) -> PyRep {
    PyRep::keyval([
        ("siteID", PyRep::String(site_id)),
        ("dungeonID", PyRep::Integer(dungeon_id as i64)),
        ("dungeonName", PyRep::String(dungeon_name)),
        ("siteCategory", PyRep::String(site_category)),
        (
            "wormholeClass",
            wormhole_class
                .map(|c| PyRep::Integer(c as i64))
                .unwrap_or(PyRep::None),
        ),
        ("currentWave", PyRep::Integer(current_wave as i64)),
        ("totalWaves", PyRep::Integer(total_waves as i64)),
        (
            "activeNpcBallIDs",
            PyRep::List(active_npc_ball_ids.into_iter().map(PyRep::Integer).collect()),
        ),
        ("probeRequired", PyRep::Bool(false)),
        ("overviewVisible", PyRep::Bool(true)),
    ])
}

async fn handle_get_combat_anomaly_instances(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (char_id, caller_sys) = resolve_exploration_space_context(ctx, session, true)?;
    if args.len() > 1 {
        let msg = "GetCombatAnomalyInstances accepts at most one solarSystemID argument";
        return Err(BridgeRpcError::with_client_error(
            "InvalidArguments",
            msg,
            eve_proto::ClientError::CustomInfo(msg.into()),
        ));
    }
    let sys_id = match args.first() {
        None | Some(PyRep::None) => caller_sys,
        Some(PyRep::Integer(raw)) => {
            let Ok(id) = u32::try_from(*raw) else {
                let msg = format!("Invalid solarSystemID {raw}");
                return Err(BridgeRpcError::with_client_error(
                    "InvalidArguments",
                    msg.clone(),
                    eve_proto::ClientError::CustomInfo(msg),
                ));
            };
            if !ctx.sde.solar_systems.contains_key(&id) {
                let msg = format!("Unknown solar system {id}");
                return Err(BridgeRpcError::with_client_error(
                    "UnknownSolarSystem",
                    msg.clone(),
                    eve_proto::ClientError::CustomInfo(msg),
                ));
            }
            id
        }
        Some(_) => {
            let msg = "solarSystemID must be an integer";
            return Err(BridgeRpcError::with_client_error(
                "InvalidArguments",
                msg,
                eve_proto::ClientError::CustomInfo(msg.into()),
            ));
        }
    };
    let is_seeded = read_pve_exploration_reg(ctx, |reg| {
        reg.seeded_systems.get(&sys_id).copied().unwrap_or(false)
            || reg.sites_by_system.get(&sys_id).is_some_and(|s| !s.is_empty())
    });
    if !is_seeded && sys_id == caller_sys {
        ensure_system_pve_sites_seeded(ctx, sys_id).await;
    }

    let list = if is_seeded || sys_id == caller_sys {
        read_pve_exploration_reg(ctx, |reg| {
            let now = now_sec(reg);
            let owned_exp_ids: std::collections::HashSet<i64> = reg
                .escalations_by_char
                .get(&char_id)
                .map(|list| {
                    list.iter()
                        .filter(|e| {
                            !crate::services::pve_expeditions::is_escalation_entry_expired(e, now)
                        })
                        .map(|e| e.instance_id)
                        .collect()
                })
                .unwrap_or_default();
            reg.sites_by_system
                .get(&sys_id)
                .map(|sites| {
                    sites
                        .iter()
                        .filter(|s| {
                            !s.probe_required
                                && !s.cleared
                                && (s.site_category != "Expedition"
                                    || s.expedition_instance_id
                                        .is_some_and(|id| owned_exp_ids.contains(&id)))
                                && !is_site_expired_in_reg(reg, s, now)
                        })
                        .map(|s| {
                            combat_anomaly_keyval(
                                s.site_id.clone(),
                                s.dungeon_id,
                                s.dungeon_name.clone(),
                                s.site_category.clone(),
                                s.wormhole_class,
                                s.current_wave,
                                s.total_waves,
                                s.active_npc_ball_ids.clone(),
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    } else {
        Vec::new()
    };
    Ok(PyRep::List(list))
}

async fn handle_enter_combat_anomaly(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (char_id, sys_id) = resolve_exploration_space_context(ctx, session, true)?;
    if args.len() > 2 {
        let msg = "EnterCombatAnomaly accepts at most two arguments";
        return Err(BridgeRpcError::with_client_error(
            "InvalidArguments",
            msg,
            eve_proto::ClientError::CustomInfo(msg.into()),
        ));
    }
    let advance_wave = match args.get(1) {
        None | Some(PyRep::None) => false,
        Some(PyRep::Bool(v)) => *v,
        Some(_) => {
            let msg = "EnterCombatAnomaly second argument must be a boolean";
            return Err(BridgeRpcError::with_client_error(
                "InvalidArguments",
                msg,
                eve_proto::ClientError::CustomInfo(msg.into()),
            ));
        }
    };
    let Some(site_id) = args
        .first()
        .and_then(|a| a.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        let msg = "Combat anomaly siteID is required";
        return Err(BridgeRpcError::with_client_error(
            "AnomalyNotFound",
            msg,
            eve_proto::ClientError::CustomInfo(msg.into()),
        ));
    };
    if advance_wave {
        if let Some(ship_id) = session.ship_id {
            ctx.validate_ship_movement(
                char_id,
                ship_id,
                crate::services::space_nav_movement_guard::MovementIntent::AccelerationGate {
                    sys_id,
                    kind: crate::services::space_nav_movement_guard::AccelerationGateKind::DedOrExploration,
                },
            )
            .await?;
        }
    }
    let site_exists = read_pve_exploration_reg(ctx, |reg| {
        let now = now_sec(reg);
        reg.sites_by_system.get(&sys_id).is_some_and(|sites| {
            find_site_by_canonical_key_any_state_in_reg(
                reg,
                sites,
                ExplorationSiteKeyRef(&site_id),
                now,
            )
            .is_some()
        })
    });
    if !site_exists {
        let msg = format!("Combat anomaly {site_id} not found in system {sys_id}");
        return Err(BridgeRpcError::with_client_error(
            "AnomalyNotFound",
            msg.clone(),
            eve_proto::ClientError::CustomInfo(msg),
        ));
    }
    if advance_wave {
        return crate::services::pve_expeditions::validate_and_advance_pve_site_room(
            ctx, session, sys_id, &site_id,
        )
        .await;
    }
    let res = crate::services::pve_expeditions::enter_combat_anomaly_site(
        ctx, char_id, sys_id, &site_id,
    );
    if res.is_ok() {
        crate::services::wormhole_pve_escalations::step_wormhole_pve_system(
            ctx, sys_id, 1.0,
        )
        .await;
    }
    res
}

fn find_site_id_by_gate_ball(
    reg: &PveExplorationRegistry,
    sys_id: SolarSystemId,
    item_id: i64,
) -> Option<String> {
    let now = now_sec(reg);
    reg.sites_by_system.get(&sys_id).and_then(|l| {
        l.iter()
            .find(|s| {
                !s.cleared
                    && !is_site_expired_in_reg(reg, s, now)
                    && s.gate_ball_id == Some(item_id)
            })
            .or_else(|| l.iter().find(|s| !s.cleared && s.gate_ball_id == Some(item_id)))
            .or_else(|| l.iter().find(|s| s.gate_ball_id == Some(item_id)))
            .map(|s| s.site_id.clone())
    })
}

async fn handle_activate_acceleration_gate(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Err(e) = ServerContext::require_character(session) {
        return Some(Err(e));
    }
    if args.len() > 2 {
        let msg = format!("{method} accepts at most two arguments");
        return Some(Err(BridgeRpcError::with_client_error(
            "InvalidArguments",
            msg.clone(),
            eve_proto::ClientError::CustomInfo(msg),
        )));
    }
    let gate_ball_arg = match args.first() {
        Some(PyRep::Integer(v)) => Some(*v),
        _ => None,
    };
    if matches!(method, "ActivateAccelerationGate" | "EnterNextDungeonRoom") {
        if let (Ok(pre_sys), Some(gate_id)) = (
            resolve_session_system_id(ctx, session),
            gate_ball_arg,
        ) {
            if let Some(res) = Box::pin(crate::services::pve_fob_observatory::try_activate_observatory_gate(
                ctx, session, pre_sys, gate_id,
            ))
            .await
            {
                return Some(res);
            }
        }
    }
    let (char_id, sys_id) = match resolve_exploration_space_context(ctx, session, false) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let gate_str_arg = args
        .first()
        .and_then(|a| a.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let has_runtime_gate = crate::services::pve_dungeon_runtime::has_runtime_gate_target(
        ctx,
        sys_id,
        gate_ball_arg,
        gate_str_arg,
    );
    if has_runtime_gate {
        if session.ship_id.is_none() {
            return Some(Err(rpc_msg_err("NoActiveShip", "Session has no active ship")));
        }
        if let Some(res) = Box::pin(crate::services::pve_dungeon_runtime::try_activate_dungeon_runtime_gate(
            ctx, session, sys_id, args,
        ))
        .await
        {
            return Some(res);
        }
    }
    let mut site_id = gate_str_arg.unwrap_or("").to_string();
    let special_id_arg = gate_ball_arg
        .or_else(|| super::r6_t1_p0_helpers::resolve_special_site_string_instance_id(ctx, &site_id));
    if let Some(item_id) = special_id_arg {
        // Integer argument: a DED acceleration gate ball in the pilot's system resolves to its site;
        // any other gate item belongs to the in-space gate / mission handlers.
        let gate_site = read_pve_exploration_reg(ctx, |reg| {
            find_site_id_by_gate_ball(reg, sys_id, item_id)
        });
        match gate_site {
            Some(sid) => {
                if session.ship_id.is_none() {
                    return Some(Err(rpc_msg_err("NoActiveShip", "Session has no active ship")));
                }
                site_id = sid;
            }
            None if super::r6_t1_p0_helpers::is_known_special_site_instance_id(ctx, item_id) => {
                let Some(ship_id) = session.ship_id else {
                    return Some(Err(rpc_msg_err("NoActiveShip", "Session has no active ship")));
                };
                if let Err(e) = ctx
                    .validate_ship_movement(
                        char_id,
                        ship_id,
                        crate::services::space_nav_movement_guard::MovementIntent::AccelerationGate {
                            sys_id,
                            kind: crate::services::space_nav_movement_guard::AccelerationGateKind::DedOrExploration,
                        },
                    )
                    .await
                {
                    return Some(Err(e));
                }
                let fleet_count = args.get(1).and_then(|a| a.as_i64()).unwrap_or(5).clamp(1, 10) as usize;
                let res = Box::pin(crate::services::pve_special_sites::complete_special_dungeon_instance(ctx, session, item_id, fleet_count)).await;
                if let Ok(ref rep) = res {
                    let is_done = rep.dict_get("completed").or_else(|| rep.dict_get("cleared")) == Some(&PyRep::Bool(true));
                    if is_done {
                        let cat = if (720_000..730_000).contains(&item_id) { "Triglavian" } else if (710_000..720_000).contains(&item_id) { "Homefront" } else { "DrifterHive" };
                        let did = rep
                            .dict_get("dungeonID")
                            .and_then(|v| v.as_i64())
                            .and_then(|v| u32::try_from(v).ok())
                            .or_else(|| {
                                crate::services::pve_special_sites::read_pve_special_reg(ctx, |reg| {
                                    reg.homefront_sites
                                        .get(&sys_id)
                                        .and_then(|l| l.iter().find(|s| s.instance_id == item_id).map(|s| s.dungeon_id))
                                        .or_else(|| {
                                            reg.triglavian_sites
                                                .get(&sys_id)
                                                .and_then(|l| l.iter().find(|s| s.instance_id == item_id).map(|s| s.dungeon_id))
                                        })
                                })
                            })
                            .unwrap_or(0);
                        ctx.quasar_gateway.progression.extra.on_system_tale_encounter_completed(sys_id, cat, did, 1, ctx.now_unix(), &ctx.quasar_gateway);
                    }
                }
                return Some(res);
            }
            None if method == "EnterNextDungeonRoom" => {
                if session.ship_id.is_none() {
                    return Some(Err(rpc_msg_err("NoActiveShip", "Session has no active ship")));
                }
                return Some(Err(rpc_msg_err(
                    "DungeonNotFound",
                    format!("Dungeon instance {item_id} not found"),
                )));
            }
            None => return None,
        }
    }
    let Some(ship_id) = session.ship_id else {
        return Some(Err(rpc_msg_err("NoActiveShip", "Session has no active ship")));
    };
    if site_id.is_empty() {
        return Some(Err(rpc_msg_err("DungeonNotFound", "Dungeon siteID is required")));
    }
    if site_id.starts_with("INC-") {
        let now_ft = ctx.now_blueos_filetime_ticks();
        let (mut inc_exists, special_initialized, locked_reason) =
            crate::services::pve_special_sites::read_pve_special_reg(ctx, |reg| {
                let mut found = false;
                let mut locked: Option<(String, String)> = None;
                for inc in reg.incursions.values() {
                    if inc.state == "Ended" || inc.state == "Withdrawing" {
                        continue;
                    }
                    for s in &inc.sites {
                        if s.solar_system_id == sys_id
                            && (s.site_id == site_id
                                || s.site_id.ends_with(&site_id)
                                || site_id.ends_with(&format!("-{}", s.dungeon_id)))
                        {
                            found = true;
                            if s.locked_until_low_influence {
                                if !inc.mothership_spawned || inc.influence > 0.10 {
                                    locked = Some((
                                        "MothershipSiteLocked".into(),
                                        format!(
                                            "Mothership HQ site {} is locked while Sansha influence ({:.2}) > 0.0",
                                            s.dungeon_name, inc.influence
                                        ),
                                    ));
                                } else if inc
                                    .lockout_until_filetime
                                    .is_some_and(|lockout| now_ft > lockout && !inc.mothership_defeated)
                                {
                                    locked = Some((
                                        "MothershipSiteLocked".into(),
                                        "Mothership HQ site is locked: lockout window has expired"
                                            .into(),
                                    ));
                                }
                            }
                            break;
                        }
                    }
                    if found {
                        break;
                    }
                }
                (found, reg.initialized, locked)
            });
        if let Some((code, msg)) = locked_reason {
            return Some(Err(rpc_msg_err(&code, msg)));
        }
        let mut spawned_for_system = false;
        if !inc_exists && !special_initialized {
            match naturally_seedable_incursion_dungeon_id(ctx, sys_id, &site_id) {
                Some(5158) => {
                    let msg = format!(
                        "Mothership HQ site {site_id} is locked while Sansha influence > 0.0"
                    );
                    return Some(Err(BridgeRpcError::with_client_error(
                        "MothershipSiteLocked",
                        msg.clone(),
                        eve_proto::ClientError::CustomInfo(msg),
                    )));
                }
                Some(_) => {
                    if let Err(e) = ctx
                        .validate_ship_movement(
                            char_id,
                            ship_id,
                            crate::services::space_nav_movement_guard::MovementIntent::AccelerationGate {
                                sys_id,
                                kind: crate::services::space_nav_movement_guard::AccelerationGateKind::DedOrExploration,
                            },
                        )
                        .await
                    {
                        return Some(Err(e));
                    }
                    Box::pin(
                        crate::services::pve_special_sites::ensure_special_sites_spawned_if_live_for_system(
                            ctx, sys_id,
                        ),
                    )
                    .await;
                    spawned_for_system = true;
                    inc_exists = crate::services::pve_special_sites::read_pve_special_reg(ctx, |reg| {
                        reg.incursions.values().any(|inc| {
                            inc.state != "Ended"
                                && inc.state != "Withdrawing"
                                && inc.sites.iter().any(|s| {
                                    s.solar_system_id == sys_id
                                        && (s.site_id == site_id
                                            || s.site_id.ends_with(&site_id)
                                            || site_id.ends_with(&format!("-{}", s.dungeon_id)))
                                })
                        })
                    });
                }
                None => {}
            }
        }
        if !inc_exists {
            let msg = format!("Incursion site {site_id} not found");
            return Some(Err(BridgeRpcError::with_client_error(
                "DungeonNotFound",
                msg.clone(),
                eve_proto::ClientError::CustomInfo(msg),
            )));
        }
        if !spawned_for_system {
            if let Err(e) = ctx
                .validate_ship_movement(
                    char_id,
                    ship_id,
                    crate::services::space_nav_movement_guard::MovementIntent::AccelerationGate {
                        sys_id,
                        kind: crate::services::space_nav_movement_guard::AccelerationGateKind::DedOrExploration,
                    },
                )
                .await
            {
                return Some(Err(e));
            }
            Box::pin(
                crate::services::pve_special_sites::ensure_special_sites_spawned_if_live_for_system(
                    ctx, sys_id,
                ),
            )
            .await;
        }
        let res = Box::pin(
            crate::services::pve_incursion_sites::process_incursion_site_completion_and_payout(
                ctx, session.character_id, session.fleet_id, sys_id, &site_id, None,
            ),
        ).await.map(|r| {
            PyRep::keyval([
                ("siteID", PyRep::String(r.site_id)), ("tier", PyRep::String(r.tier)),
                ("fleetSize", PyRep::Integer(r.fleet_size as i64)), ("fleetScaleFactor", PyRep::Real(r.fleet_scale_factor)),
                ("iskPerPilot", PyRep::Integer(r.isk_per_pilot)), ("concordLpPerPilot", PyRep::Integer(r.concord_lp_per_pilot)),
                ("influence", PyRep::Real(r.influence)), ("state", PyRep::String(r.state)),
                ("mothershipSpawned", PyRep::Bool(r.mothership_spawned)), ("incursionEnded", PyRep::Bool(r.incursion_ended)),
            ])
        });
        return Some(res);
    }
    let site_exists = read_pve_exploration_reg(ctx, |reg| {
        let now = now_sec(reg);
        reg.sites_by_system.get(&sys_id).is_some_and(|l| {
            find_site_by_canonical_key_any_state_in_reg(reg, l, ExplorationSiteKeyRef(&site_id), now)
                .is_some()
        })
    });
    if !site_exists {
        let msg = format!("Dungeon site {site_id} not found");
        return Some(Err(BridgeRpcError::with_client_error(
            "DungeonNotFound",
            msg.clone(),
            eve_proto::ClientError::CustomInfo(msg),
        )));
    }
    Some(
        Box::pin(crate::services::pve_expeditions::validate_and_advance_pve_site_room(
            ctx, session, sys_id, &site_id,
        ))
        .await,
    )
}

async fn handle_get_dungeon_info(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, sys_id) = resolve_exploration_space_context(ctx, session, false)?;
    if args.len() > 1 {
        let msg = "GetDungeonInfo accepts a single siteID argument";
        return Err(BridgeRpcError::with_client_error(
            "InvalidArguments",
            msg,
            eve_proto::ClientError::CustomInfo(msg.into()),
        ));
    }
    let Some(site_id) = args
        .first()
        .and_then(|a| a.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        let msg = "Dungeon siteID is required";
        return Err(BridgeRpcError::with_client_error(
            "DungeonNotFound",
            msg,
            eve_proto::ClientError::CustomInfo(msg.into()),
        ));
    };
    read_pve_exploration_reg(ctx, |reg| {
        let now = now_sec(reg);
        let Some(site) = reg.sites_by_system.get(&sys_id).and_then(|l| {
            let key = ExplorationSiteKeyRef(&site_id);
            find_site_by_canonical_key_any_state_in_reg(reg, l, key, now)
        }) else {
            let msg = format!("Dungeon site {site_id} not found");
            return Err(BridgeRpcError::with_client_error(
                "DungeonNotFound",
                msg.clone(),
                eve_proto::ClientError::CustomInfo(msg),
            ));
        };
        Ok(PyRep::keyval([
            ("siteID", PyRep::String(site.site_id.clone())),
            ("dungeonID", PyRep::Integer(site.dungeon_id as i64)),
            ("dungeonName", PyRep::String(site.dungeon_name.clone())),
            ("siteCategory", PyRep::String(site.site_category.clone())),
            ("currentRoom", PyRep::Integer(site.current_room as i64)),
            ("totalRooms", PyRep::Integer(site.total_rooms as i64)),
            ("currentWave", PyRep::Integer(site.current_wave as i64)),
            ("totalWaves", PyRep::Integer(site.total_waves as i64)),
            ("sleeperRiftStabilized", PyRep::Bool(site.sleeper_rift_stabilized)),
            ("sleeperSentryDisabled", PyRep::Bool(site.sleeper_sentry_disabled)),
            ("sleeperGasCloudActive", PyRep::Bool(site.sleeper_gas_cloud_active)),
            ("cleared", PyRep::Bool(site.cleared)),
        ]))
    })
}

fn handle_get_escalating_path_details(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if !args.is_empty() {
        return Err(BridgeRpcError::user_error("InvalidArguments"));
    }
    Ok(crate::services::pve_expeditions::get_my_escalating_path_details_rowset(ctx, char_id))
}

fn handle_get_escalating_path_dungeons_entered(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let invalid_err = || {
        let msg = "Invalid escalation query arguments";
        Err(BridgeRpcError::with_client_error(
            "InvalidArguments",
            msg,
            eve_proto::ClientError::CustomInfo(msg.into()),
        ))
    };
    let instance_filter = if service == "scanSvc" {
        if !args.is_empty() {
            return invalid_err();
        }
        None
    } else {
        if args.len() > 1 {
            return invalid_err();
        }
        match args.first() {
            None | Some(PyRep::None) => None,
            Some(PyRep::Integer(id)) if *id > 0 => Some(*id),
            Some(_) => return invalid_err(),
        }
    };
    let entered = read_pve_exploration_reg(ctx, |reg| {
        if let Some(target_inst) = instance_filter {
            let now = now_sec(reg);
            let mut seen = std::collections::HashSet::new();
            reg.escalations_by_char
                .get(&char_id)
                .into_iter()
                .flatten()
                .filter(|e| {
                    e.instance_id == target_inst
                        && !crate::services::pve_expeditions::is_escalation_entry_expired(e, now)
                })
                .flat_map(|e| e.dungeons_entered.iter().copied())
                .filter(|d| seen.insert(*d))
                .map(|d| PyRep::Integer(d as i64))
                .collect()
        } else {
            crate::services::pve_expeditions::get_escalating_path_dungeons_entered(reg, char_id)
        }
    });
    Ok(PyRep::List(entered))
}

use crate::router::rpc_registry::{
    rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry, RpcSceneClass,
};

fn dispatch_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(handle_pve_exploration(ctx, session, service, method, args, kwargs))
}

fn dispatch_activate_acceleration_gate<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    bound_entry: Option<&'a crate::router::BoundObjectEntry>,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(async move {
        if let Some(res) =
            handle_pve_exploration(ctx, session, service, method, args, kwargs).await
        {
            return Some(res);
        }
        if service == "keeper" {
            if let Some(res) =
                crate::services::agents_missions::agents_standings_arcs::handle_extended_with_bound(
                    ctx,
                    session,
                    bound_entry,
                    service,
                    method,
                    args,
                    kwargs,
                )
                .await
            {
                return Some(res);
            }
        }
        crate::services::fleet_social::fleet_social_scanning::handle_scanning_and_wormholes(
            ctx, session, service, method, args, kwargs,
        )
        .await
    })
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("beyonce", "ActivateAccelerationGate", RpcDispatchHandler::BoundAsync(dispatch_activate_acceleration_gate)),
    rpc_entry("keeper", "ActivateAccelerationGate", RpcDispatchHandler::BoundAsync(dispatch_activate_acceleration_gate)),
    rpc_entry("scanSvc", "GetCombatAnomalyInstances", RpcDispatchHandler::Async(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
    rpc_entry("scanSvc", "EnterCombatAnomaly", RpcDispatchHandler::Async(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
    rpc_entry("keeper", "GetDungeonShipRestrictions", RpcDispatchHandler::Async(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
    rpc_entry("keeper", "CanEnterDungeonInCurrentShip", RpcDispatchHandler::Async(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
    rpc_entry("keeper", "EnterNextDungeonRoom", RpcDispatchHandler::Async(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
    rpc_entry("keeper", "GetDungeonInfo", RpcDispatchHandler::Async(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
    rpc_entry("keeper", "GetDungeonObjectivesData", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("dungeonExplorationMgr", "GetMyEscalatingPathDetails", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("scanSvc", "GetMyEscalatingPathDetails", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("dungeonExplorationMgr", "GetEscalations", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("dungeonExplorationMgr", "GetEscalatingPathDungeonsEntered", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("scanSvc", "GetEscalatingPathDungeonsEntered", RpcDispatchHandler::Async(dispatch_rpc)),
];

#[cfg(test)]
#[path = "pve_exploration_tests.rs"]
mod pve_exploration_tests;

#[cfg(test)]
#[path = "pve_exploration_edges_tests.rs"]
mod pve_exploration_edges_tests;
#[cfg(test)]
#[path = "pve_exploration_edges_b4_tests.rs"]
mod pve_exploration_edges_b4_tests;
