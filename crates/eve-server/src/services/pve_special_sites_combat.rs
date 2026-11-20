//! `pve_special_sites_combat` — Special-Site NPC Destruction Handlers (Homefront Operations,
//! Triglavian Sites, Drifter Hives, Abyssal Bioadaptive Caches, and CRAB Rogue Drone Waves).
//!
// parity: feat:pve.homefront-ops, feat:pve.drifter-hives, feat:pve.triglavian-sites

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_incursion_sites::make_site_npc_ball;
use crate::services::pve_special_sites::{
    build_special_site_npc_combat_state, deposit_loot_into_ship_or_container_in_snapshot,
    with_pve_special_reg, with_pve_special_reg_db, DrifterHiveRecord,
};
use eve_db::lock_rank::AnyRwLock;
use eve_db::{IskCents, WorldRepository};
use eve_sde::SolarSystemId;
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};

type CharacterId = i64;

const SEASONAL_DETAILS_NOTE_ID: i64 = -8_300_000_000_001;
const SEASONAL_COOLDOWN_NOTE_ID: i64 = -8_300_000_000_002;
pub const SEASONAL_RESPAWN_COOLDOWN_SECS: u64 = 1_800;

fn default_seasonal_site_position() -> [f64; 3] {
    [1.5e11, 2.0e10, -1.0e11]
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SeasonalSiteDetails {
    pub instance_id: i64,
    pub season_theme: String,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    #[serde(default)]
    pub faction_id: u32,
    pub boss_type_id: u32,
    pub escort_type_id: u32,
    pub relay_ball_id: i64,
    pub relay_type_id: u32,
    pub relay_hacked: bool,
    pub exploration_site_id: String,
    pub crate_type_id: u32,
    pub crate_loot_pool: Vec<(u32, i64)>,
    #[serde(default)]
    pub boss_defeated: bool,
    #[serde(default)]
    pub defeated_escort_ball_ids: Vec<i64>,
    #[serde(default = "default_seasonal_site_position")]
    pub site_position: [f64; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SeasonalCooldownStore {
    #[serde(default)]
    until_by_system: BTreeMap<SolarSystemId, u64>,
    #[serde(default)]
    spawn_seq_by_system: BTreeMap<SolarSystemId, usize>,
}

fn load_seasonal_cooldown_store(snap: &eve_db::WorldStateSnapshot) -> SeasonalCooldownStore {
    crate::services::pve_special_sites::load_json_owner_note(snap, SEASONAL_COOLDOWN_NOTE_ID)
}

fn save_seasonal_cooldown_store(
    snap: &mut eve_db::WorldStateSnapshot,
    store: &SeasonalCooldownStore,
) {
    crate::services::pve_special_sites::save_json_owner_note(
        snap,
        SEASONAL_COOLDOWN_NOTE_ID,
        "seasonal_cooldowns",
        store.until_by_system.is_empty() && store.spawn_seq_by_system.is_empty(),
        store,
    );
}

pub fn load_seasonal_site_details_map(
    snap: &eve_db::WorldStateSnapshot,
) -> BTreeMap<i64, SeasonalSiteDetails> {
    crate::services::pve_special_sites::load_json_owner_note(snap, SEASONAL_DETAILS_NOTE_ID)
}

fn save_seasonal_site_details_map(
    snap: &mut eve_db::WorldStateSnapshot,
    map: &BTreeMap<i64, SeasonalSiteDetails>,
) {
    crate::services::pve_special_sites::save_json_owner_note(
        snap,
        SEASONAL_DETAILS_NOTE_ID,
        "seasonal_site_details",
        map.is_empty(),
        map,
    );
}

pub fn get_seasonal_site_details(
    ctx: &ServerContext,
    instance_id: i64,
) -> Option<SeasonalSiteDetails> {
    ctx.db
        .with_snapshot(|snap| load_seasonal_site_details_map(snap).remove(&instance_id))
}

fn mutate_seasonal_exploration_reg_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    now_sec: u64,
    f: impl FnOnce(&mut crate::services::pve_exploration::PveExplorationRegistry) -> bool,
) -> Result<(), String> {
    use crate::services::pve_exploration::{PveExplorationRegistry, PVE_EXPLORATION_NOTE_ID};

    if let Some(note) = snap.owner_notes.get_mut(&PVE_EXPLORATION_NOTE_ID) {
        let mut exp_reg = serde_json::from_str::<PveExplorationRegistry>(&note.note)
            .map_err(|e| format!("failed to parse exploration registry: {e}"))?;
        if f(&mut exp_reg) {
            note.note = serde_json::to_string(&exp_reg)
                .map_err(|e| format!("failed to serialize exploration registry: {e}"))?;
            note.timestamp_unix = now_sec;
        }
    } else {
        let mut exp_reg = PveExplorationRegistry::default();
        if f(&mut exp_reg) {
            let new_json = serde_json::to_string(&exp_reg)
                .map_err(|e| format!("failed to serialize exploration registry: {e}"))?;
            snap.owner_notes.insert(
                PVE_EXPLORATION_NOTE_ID,
                eve_db::OwnerNoteRecord {
                    note_id: PVE_EXPLORATION_NOTE_ID,
                    character_id: -82,
                    item_id: 0,
                    label: "pve_exploration_registry".into(),
                    note: new_json,
                    timestamp_unix: now_sec,
                },
            );
        }
    }
    Ok(())
}

fn seasonal_system_lock(
    db: &Arc<WorldRepository>,
    sys_id: SolarSystemId,
) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<
        std::sync::Mutex<HashMap<(usize, SolarSystemId), Arc<tokio::sync::Mutex<()>>>>,
    > = OnceLock::new();
    let map = LOCKS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let key = (Arc::as_ptr(db) as usize, sys_id);
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    if guard.len() > 256 {
        guard.retain(|_, lock| Arc::strong_count(lock) > 1);
    }
    guard
        .entry(key)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

fn seasonal_site_valid_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    sys_id: SolarSystemId,
    site: &crate::services::pve_special_sites::SeasonalEventSiteRecord,
    active_season_id: Option<i64>,
    now_ft: i64,
    now_sec: u64,
) -> bool {
    if site.completed
        || !active_season_id.is_some_and(|sid| site.season_id == sid)
        || now_ft < site.window_start_ft
        || now_ft > site.window_end_ft
    {
        return false;
    }
    let anom_id = format!("dng-{}-{}", site.dungeon_id, site.instance_id);
    snap.cosmic_anomalies.get(&sys_id).is_some_and(|anoms| {
        anoms.iter().any(|a| {
            a.anomaly_id == anom_id
                && a.anomaly_type == "SeasonalEventSite"
                && !a.cleared
                && now_sec < a.created_unix.saturating_add(a.lifetime_seconds)
        })
    })
}

fn remove_seasonal_exploration_sites_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    sys_id: SolarSystemId,
    now_sec: u64,
    exp_ids: &[String],
) -> Result<(), String> {
    if exp_ids.is_empty() {
        return Ok(());
    }
    mutate_seasonal_exploration_reg_in_snap(snap, now_sec, |reg| {
        let mut changed = false;
        if let Some(sites) = reg.sites_by_system.get_mut(&sys_id) {
            let before = sites.len();
            sites.retain(|s| !exp_ids.contains(&s.site_id));
            if sites.len() != before {
                changed = true;
            }
        }
        changed
    })
}

// DERIVED: Theme classification and SDE keyword mapping when FSD season metadata provides only
// season_id and season_name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SeasonTheme {
    CrimsonHarvest,
    WinterNexus,
    GuardiansGala,
    LiberationDay,
}

impl SeasonTheme {
    fn from_season(season: &eve_db::SeasonConfig) -> Self {
        let lower = season.season_name.to_lowercase();
        if lower.contains("winter") || lower.contains("wightstorm") || season.season_id == 2 {
            Self::WinterNexus
        } else if lower.contains("gala") || lower.contains("guardian") {
            Self::GuardiansGala
        } else if lower.contains("liberation") || lower.contains("minmatar") {
            Self::LiberationDay
        } else {
            Self::CrimsonHarvest
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::CrimsonHarvest => "Crimson Harvest",
            Self::WinterNexus => "Winter Nexus",
            Self::GuardiansGala => "Guardian's Gala",
            Self::LiberationDay => "Liberation Day",
        }
    }

    fn dungeon_keywords(self) -> &'static [&'static str] {
        match self {
            Self::CrimsonHarvest => &["crimson", "tetrimon"],
            Self::WinterNexus => &["wightstorm", "winter"],
            Self::GuardiansGala => &["gala"],
            Self::LiberationDay => &["liberation"],
        }
    }

    fn crate_keywords(self) -> &'static [&'static str] {
        match self {
            Self::CrimsonHarvest => &["crimson", "tetrimon", "halloween", "headhunter"],
            Self::WinterNexus => &["winter", "wightstorm", "yoiul", "chilling"],
            Self::GuardiansGala => &["gala"],
            Self::LiberationDay => &["liberation", "minmatar"],
        }
    }

    fn npc_keywords_for_dungeon(
        self,
        dungeon_name: &str,
    ) -> (&'static [&'static str], &'static [&'static str]) {
        let lower_dname = dungeon_name.to_lowercase();
        match self {
            Self::CrimsonHarvest if lower_dname.contains("tetrimon") => {
                (&["tetrimon"], &["tetrimon"])
            }
            Self::CrimsonHarvest => (&["harvest", "blood raider", "tetrimon"], &["crimson harvest", "tetrimon"]),
            Self::WinterNexus => (&["direwight", "frostwight", "wightstorm", "sansha"], &["wightstorm"]),
            Self::GuardiansGala => (&["gala", "guardian angels"], &["gala"]),
            Self::LiberationDay => (&["republic ", "minmatar", "tetrimon"], &["network node", "com tower", "data"]),
        }
    }
}

struct ResolvedSeasonalSpec {
    theme: SeasonTheme,
    dungeon_id: u32,
    dungeon_name: String,
    faction_id: u32,
    boss_type_id: u32,
    escort_type_id: u32,
    relay_type_id: u32,
    crate_type_id: u32,
    crate_loot_pool: Vec<(u32, i64)>,
    reward_isk: i64,
    reward_points: i64,
}

fn first_known_type(sde: &eve_sde::SdeRegistry, candidates: &[u32]) -> Option<u32> {
    candidates
        .iter()
        .copied()
        .find(|tid| sde.types.contains_key(tid))
}

fn find_sde_npc_by_keywords_and_groups(
    sde: &eve_sde::SdeRegistry,
    keywords: &[&str],
    group_tiers: &[u32],
    exclude_keywords: &[&str],
) -> Option<u32> {
    for &gid in group_tiers {
        let mut best: Option<u32> = None;
        for t in sde.types.values() {
            if t.category_id != 11 || t.group_id != gid {
                continue;
            }
            let lower = t.name.to_lowercase();
            if exclude_keywords.iter().any(|ex| lower.contains(ex)) {
                continue;
            }
            if keywords.is_empty() || keywords.iter().any(|kw| lower.contains(kw)) {
                best = Some(best.map_or(t.type_id, |prev| prev.min(t.type_id)));
            }
        }
        if best.is_some() {
            return best;
        }
    }
    None
}

fn find_sde_relay_type(sde: &eve_sde::SdeRegistry, keywords: &[&str]) -> Option<u32> {
    for allow_any_name in [false, true] {
        let mut best: Option<u32> = None;
        for t in sde.types.values() {
            if t.category_id != 11 || !matches!(t.group_id, 306 | 1207) {
                continue;
            }
            let lower = t.name.to_lowercase();
            if lower.contains("old content") {
                continue;
            }
            if allow_any_name || keywords.iter().any(|kw| lower.contains(kw)) {
                best = Some(best.map_or(t.type_id, |prev| prev.min(t.type_id)));
            }
        }
        if best.is_some() {
            return best;
        }
    }
    None
}

// DERIVED: Priority tiers for seasonal booster selection (theme/cerebral > combat drugs >
// any published group_id=303 booster) when crate loot tables lack tagged seasonal boosters.
fn resolve_seasonal_booster_type_id(
    sde: &eve_sde::SdeRegistry,
    season_theme: &str,
) -> Option<u32> {
    let lower_theme = season_theme.to_lowercase();
    let theme_terms: &[&str] = if lower_theme.contains("winter") {
        &["winter", "wightstorm", "yoiul", "cerebral"]
    } else if lower_theme.contains("gala") {
        &["gala", "cerebral"]
    } else if lower_theme.contains("liberation") {
        &["liberation", "cerebral"]
    } else {
        &["crimson", "tetrimon", "cerebral"]
    };
    let is_valid_booster = |t: &eve_sde::InvType| -> bool {
        if t.group_id != 303 || !t.published {
            return false;
        }
        let lower = t.name.to_lowercase();
        !lower.contains("expired") && !lower.contains("qa ") && !lower.contains("serenity")
    };
    let mut theme_preferred: Option<u32> = None;
    let mut combat_preferred: Option<u32> = None;
    let mut fallback: Option<u32> = None;
    for t in sde.types.values() {
        if !is_valid_booster(t) {
            continue;
        }
        fallback = Some(fallback.map_or(t.type_id, |prev| prev.min(t.type_id)));
        let lower = t.name.to_lowercase();
        if theme_terms.iter().any(|kw| lower.contains(kw)) {
            theme_preferred = Some(theme_preferred.map_or(t.type_id, |prev| prev.min(t.type_id)));
        } else if lower.contains("pyrolancea") || lower.contains("blue pill") {
            combat_preferred = Some(combat_preferred.map_or(t.type_id, |prev| prev.min(t.type_id)));
        }
    }
    theme_preferred.or(combat_preferred).or(fallback)
}

fn resolve_loot_container_type_id(sde: &eve_sde::SdeRegistry) -> u32 {
    if let Some(tid) = sde
        .types
        .values()
        .filter(|t| {
            t.group_id == 12
                && t.category_id == 2
                && t.name.to_lowercase().contains("cargo container")
        })
        .map(|t| t.type_id)
        .min()
    {
        return tid;
    }
    first_known_type(sde, &[23, 3293]).unwrap_or(23)
}

fn resolve_seasonal_event_spec(
    sde: &eve_sde::SdeRegistry,
    season: &eve_db::SeasonConfig,
    sys_id: SolarSystemId,
) -> ResolvedSeasonalSpec {
    let theme = SeasonTheme::from_season(season);
    let dng_keywords = theme.dungeon_keywords();
    let mut matching_dungeons: Vec<(u32, String, Option<u32>, Option<u32>, Option<u32>)> = sde
        .dungeons
        .values()
        .filter(|d| {
            if !d.archetype_id.is_some_and(|a| matches!(a, 24 | 31 | 45)) {
                return false;
            }
            let lower = d.name.to_lowercase();
            let excluded = lower.contains("do not translate")
                || lower.contains("spawner")
                || lower.contains("arena")
                || lower.contains("proving")
                || lower.contains("supply depot")
                || lower.contains("drudge")
                || lower.contains("anchorage")
                || lower.contains("hub");
            !excluded && dng_keywords.iter().any(|kw| lower.contains(kw))
        })
        .map(|d| {
            (
                d.dungeon_id,
                d.name.trim().to_string(),
                d.faction_id,
                d.difficulty,
                d.archetype_id,
            )
        })
        .collect();
    if matching_dungeons.is_empty() {
        matching_dungeons = sde
            .dungeons
            .values()
            .filter(|d| d.archetype_id == Some(45))
            .map(|d| {
                (
                    d.dungeon_id,
                    d.name.trim().to_string(),
                    d.faction_id,
                    d.difficulty,
                    d.archetype_id,
                )
            })
            .collect();
    }
    matching_dungeons.sort_by_key(|(did, _, _, _, _)| *did);

    let (dungeon_id, dungeon_name, dungeon_faction_id, dungeon_difficulty, dungeon_archetype) =
        if !matching_dungeons.is_empty() {
            let pick_idx = (sys_id as usize + season.season_id.unsigned_abs() as usize)
                % matching_dungeons.len();
            matching_dungeons[pick_idx].clone()
        } else {
            (
                8_923,
                format!("{} Site", theme.label()),
                None,
                Some(3),
                Some(24),
            )
        };

    let (npc_keywords, relay_keywords) = theme.npc_keywords_for_dungeon(&dungeon_name);
    let npc_exclude = &["relay", "node", "array", "tower", "databank", "hub", "can", "sentry", "spawn"];
    let boss_type_id = find_sde_npc_by_keywords_and_groups(
        sde,
        npc_keywords,
        &[1667, 1666, 553, 552],
        npc_exclude,
    )
    .or_else(|| find_sde_npc_by_keywords_and_groups(sde, &[], &[1667, 553, 552], npc_exclude))
    .unwrap_or(56_676);

    let escort_type_id = find_sde_npc_by_keywords_and_groups(
        sde,
        npc_keywords,
        &[1568, 1664, 1665, 1666, 550, 551, 552],
        npc_exclude,
    )
    .or_else(|| find_sde_npc_by_keywords_and_groups(sde, &[], &[1568, 1664, 550, 551], npc_exclude))
    .unwrap_or(boss_type_id);

    let relay_type_id = find_sde_relay_type(sde, relay_keywords)
        .unwrap_or_else(|| first_known_type(sde, &[23]).unwrap_or(23));

    // Avoid poisoning OnceLock tables inside `season_fsd` when synthetic unit-test SDEs omit full FSD data.
    let (crate_type_id, crate_loot_pool, reward_points) = if sde.types.len() > 100 {
        let crate_tables = crate::services::season_fsd::fsd_crate_loot_tables(sde);
        let crate_kws = theme.crate_keywords();
        let picked_crate = crate_tables
            .keys()
            .copied()
            .find(|tid| {
                sde.types.get(tid).is_some_and(|t| {
                    let lower = t.name.to_lowercase();
                    t.published
                        && !lower.contains("expired")
                        && crate_kws.iter().any(|kw| lower.contains(kw))
                })
            })
            .or_else(|| crate_tables.keys().copied().next())
            .or_else(|| {
                sde.types
                    .values()
                    .filter(|t| {
                        t.published
                            && t.group_id == 1194
                            && !t.name.to_lowercase().contains("expired")
                    })
                    .map(|t| t.type_id)
                    .min()
            })
            .unwrap_or(34);
        let pool: Vec<(u32, i64)> = crate_tables
            .get(&picked_crate)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|(tid, qty, _)| *qty > 0 && sde.types.contains_key(tid))
                    .take(2)
                    .map(|(tid, qty, _)| (*tid, *qty))
                    .collect()
            })
            .unwrap_or_default();
        let fsd_defs = crate::services::season_fsd::fsd_challenge_definitions(sde);
        let did_i64 = i64::from(dungeon_id);
        let pts = fsd_defs
            .iter()
            .find(|d| {
                d.points > 0
                    && d.event_conditions
                        .values()
                        .any(|vals| vals.contains(&did_i64))
            })
            .or_else(|| {
                fsd_defs.iter().find(|d| {
                    !d.is_dormant
                        && d.points > 0
                        && matches!(d.event_type.as_str(), "destroyHostileNPC" | "completeDungeon")
                })
            })
            .or_else(|| fsd_defs.iter().find(|d| !d.is_dormant && d.points > 0))
            .map(|d| d.points)
            .unwrap_or(30);
        (picked_crate, pool, pts)
    } else {
        let picked_crate = sde
            .types
            .values()
            .filter(|t| t.published && t.group_id == 1194)
            .map(|t| t.type_id)
            .min()
            .or_else(|| first_known_type(sde, &[2048, 34]))
            .unwrap_or(34);
        (picked_crate, Vec::new(), 30)
    };

    // DERIVED: Integer security-band multipliers (highsec 1x, lowsec 3/2x, null/WH 2x),
    // baseline completion ISK payout (1.5M + difficulty * 250k ISK), archetype difficulty
    // fallbacks, and pirate faction fallbacks when SDE dungeon rows omit them.
    let sec = sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.5);
    let (sec_num, sec_den): (i64, i64) = if sec >= 0.45 {
        (1, 1)
    } else if sec > 0.0 {
        (3, 2)
    } else {
        (2, 1)
    };
    let difficulty = dungeon_difficulty
        .unwrap_or(match dungeon_archetype {
            Some(31) => 4,
            Some(45) => 2,
            _ => 3,
        })
        .clamp(1, 5);
    let base_isk: i64 = 1_500_000 + i64::from(difficulty) * 250_000;
    let reward_isk = base_isk * sec_num / sec_den;
    let faction_id = dungeon_faction_id.unwrap_or(match theme {
        SeasonTheme::CrimsonHarvest => 500_012,
        SeasonTheme::WinterNexus => 500_019,
        SeasonTheme::GuardiansGala => 500_020,
        SeasonTheme::LiberationDay => 500_011,
    });

    ResolvedSeasonalSpec {
        theme,
        dungeon_id,
        dungeon_name,
        faction_id,
        boss_type_id,
        escort_type_id,
        relay_type_id,
        crate_type_id,
        crate_loot_pool,
        reward_isk,
        reward_points,
    }
}

pub fn get_drifter_hive_for_system(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> Option<DrifterHiveRecord> {
    with_pve_special_reg(ctx, |reg| reg.drifter_hives.get(&sys_id).cloned())
}

pub fn get_all_drifter_hives(ctx: &ServerContext) -> Vec<DrifterHiveRecord> {
    with_pve_special_reg(ctx, |reg| reg.drifter_hives.values().cloned().collect())
}

pub fn notify_special_site_account_changes(
    db: &WorldRepository,
    pending_notifications: &dyn AnyRwLock<
        std::collections::HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>,
    >,
    notify_tx: &tokio::sync::broadcast::Sender<(
        i64,
        crate::router::cache_notifications::PendingMachoNotification,
    )>,
    changes: &[(i64, f64)],
) {
    if changes.is_empty() {
        return;
    }
    let now_ft = db.clock().now_blueos_filetime_ticks();
    for &(char_id, new_balance_isk) in changes {
        crate::router::notify_registry::emit_typed_target_to_char_with_id_type(
            Some(pending_notifications),
            Some(notify_tx),
            char_id,
            None,
            <crate::router::notify_registry::OnAccountChange as crate::router::notify_registry::TypedNotification>::DEFAULT_ID_TYPE,
            now_ft,
            crate::router::notify_registry::OnAccountChange {
                account_key: "cash".into(),
                owner_id: char_id,
                balance: new_balance_isk,
            },
        );
    }
}

pub async fn on_special_site_npc_destroyed_core(
    _systems: &Arc<crate::system_actor::SystemRegistry>,
    db: &Arc<WorldRepository>,
    _sde: &Arc<eve_sde::SdeRegistry>,
    solar_system_id: SolarSystemId,
    npc_ball_id: i64,
    killer_char_id: Option<CharacterId>,
) -> Vec<(CharacterId, f64)> {
    let now_sec = db.clock().now_unix_secs();
    let now_ft = db.clock().now_blueos_filetime_ticks();
    let mut acct_changes = Vec::new();

    // 0. Delegate Homefront Operations entities & structures (even when killer_char_id is None,
    //    e.g., protectee/smuggler/dread destruction or NPC/environment deaths).
    if let Some(hf_accts) = crate::services::pve_homefront_sites::on_homefront_entity_destroyed_core(
        _systems,
        db,
        _sde,
        solar_system_id,
        npc_ball_id,
        killer_char_id,
    )
    .await
    {
        return hf_accts;
    }

    // 1. Check Triglavian sites
    let mut trig_escorts_to_remove = Vec::new();
    let trig_res = with_pve_special_reg_db(db, now_sec, |reg| {
        let mut found = None;
        if let Some(list) = reg.triglavian_sites.get_mut(&solar_system_id) {
            for site in list.iter_mut() {
                if site.boss_npc_ball_id == npc_ball_id && !site.completed {
                    site.completed = true;
                    trig_escorts_to_remove = site.escort_ball_ids.clone();
                    let (g_fac, l_fac) = if site.faction_owner_id == 500_027 {
                        (500_026_i64, 500_027_i64)
                    } else {
                        (500_027_i64, 500_026_i64)
                    };
                    found = Some((site.instance_id, site.base_isk, site.survey_db_drop, g_fac, l_fac));
                    break;
                }
            }
        }
        found.map(|(inst_id, base_isk, survey_db_drop, g_fac, l_fac)| {
            reg.next_seq += 1;
            let loot_id = 1_899_500_000 + reg.next_seq;
            (inst_id, base_isk, survey_db_drop, g_fac, l_fac, loot_id)
        })
    });

    if let Some((_inst_id, base_isk, db_drop, g_fac, l_fac, loot_id)) = trig_res {
        if !trig_escorts_to_remove.is_empty() {
            let sys_handle = _systems.get_or_spawn(solar_system_id);
            for ebid in trig_escorts_to_remove {
                sys_handle.try_remove_ball(ebid);
            }
        }
        if let Some(cid) = killer_char_id {
            let spawned_cid = db.with_mut_snapshot(|snap| {
                let (new_bal_opt, _) =
                    crate::services::pve_special_sites_ops::apply_triglavian_standings_and_isk(
                        snap,
                        cid,
                        g_fac,
                        l_fac,
                        base_isk,
                        "Triglavian Site Payout",
                    );
                if let Some(nb) = new_bal_opt {
                    acct_changes.push((cid, nb));
                }
                let ship_id = snap
                    .characters
                    .get(&cid)
                    .map(|c| c.active_ship_id)
                    .unwrap_or(0);
                deposit_loot_into_ship_or_container_in_snapshot(
                    snap,
                    _sde,
                    cid,
                    solar_system_id,
                    ship_id,
                    &[
                        (loot_id, 48_121, db_drop as i32),
                        (loot_id + 1_000_000, 30_747, 2),
                    ],
                )
            });
            if let Some(container_id) = spawned_cid {
                let sys_handle = _systems.get_or_spawn(solar_system_id);
                let _ = sys_handle
                    .spawn_ball(make_site_npc_ball(container_id, 23, DVec3::new(10_500.0, 0.0, 0.0)))
                    .await;
            }
        }
        return acct_changes;
    }

    // 2. Check Drifter Hives
    let drifter_res = with_pve_special_reg_db(db, now_sec, |reg| {
        let mut spawn_next = None;
        let mut hive_loot = None;
        if let Some(hive) = reg.drifter_hives.get_mut(&solar_system_id) {
            if hive.warden_npc_ball_id == npc_ball_id && !hive.cleared {
                hive.cleared = true;
                hive.current_room = hive.total_rooms;
                if let Some(r) = hive.rooms.iter_mut().find(|r| r.room_index == hive.total_rooms) {
                    r.cleared = true;
                    if !r.destroyed_npc_ball_ids.contains(&npc_ball_id) {
                        r.destroyed_npc_ball_ids.push(npc_ball_id);
                    }
                }
                hive_loot = Some(hive.element_type_id);
            } else if !hive.cleared {
                let cur_room_idx = hive.current_room;
                if let Some(r) = hive
                    .rooms
                    .iter_mut()
                    .find(|r| r.room_index == cur_room_idx && !r.cleared)
                {
                    if r.npc_ball_ids.contains(&npc_ball_id) {
                        if !r.destroyed_npc_ball_ids.contains(&npc_ball_id) {
                            r.destroyed_npc_ball_ids.push(npc_ball_id);
                        }
                        let wave_cleared = r
                            .npc_ball_ids
                            .iter()
                            .all(|bid| r.destroyed_npc_ball_ids.contains(bid));
                        if wave_cleared {
                            r.cleared = true;
                            hive.current_room += 1;
                            let next_room_idx = hive.current_room;
                            if let Some(next_r) = hive
                                .rooms
                                .iter()
                                .find(|nr| nr.room_index == next_room_idx)
                                .cloned()
                            {
                                spawn_next = Some(next_r);
                            }
                        }
                    }
                }
            }
        }
        (
            spawn_next,
            hive_loot.map(|elem_tid| {
                crate::services::pve_special_sites_ops::allocate_drifter_hive_bonus_loot(
                    reg, elem_tid,
                )
            }),
        )
    });

    // Abyssal Bioadaptive Cache destroyed: wreck + loot transfer
    crate::services::pve_special_sites_abyssal::on_abyssal_cache_destroyed_core(
        _systems,
        db,
        solar_system_id,
        npc_ball_id,
        killer_char_id,
    )
    .await;
    // CRAB Rogue Drone wave NPC destroyed: advance next wave if cleared early
    crate::services::pve_crab_beacons::on_crab_wave_npc_destroyed(
        _systems,
        db,
        _sde,
        solar_system_id,
        npc_ball_id,
    )
    .await;

    let (spawn_next, hive_loot) = drifter_res;
    if let Some(next_room) = spawn_next {
        let sys_handle = _systems.get_or_spawn(solar_system_id);
        for (&bid, &tid) in next_room.npc_ball_ids.iter().zip(&next_room.npc_type_ids) {
            _systems.claim_special_site_ball(solar_system_id, bid);
            let cs = build_special_site_npc_combat_state(_sde, tid);
            let _ = sys_handle
                .spawn_ball_with_combat(
                    make_site_npc_ball(bid, tid, DVec3::new(10_000.0, 0.0, 0.0)),
                    cs,
                )
                .await;
        }
        if next_room.beacon_ball_id != 0 {
            _systems.claim_special_site_ball(solar_system_id, next_room.beacon_ball_id);
            let _ = sys_handle
                .spawn_ball(make_site_npc_ball(
                    next_room.beacon_ball_id,
                    42_538,
                    DVec3::new(12_000.0, 0.0, 0.0),
                ))
                .await;
        }
    }

    if let Some(hive_items) = hive_loot {
        if let Some(cid) = killer_char_id {
            let spawned_cid = db.with_mut_snapshot(|snap| {
                let ship_id = snap
                    .characters
                    .get(&cid)
                    .map(|c| c.active_ship_id)
                    .unwrap_or(0);
                deposit_loot_into_ship_or_container_in_snapshot(
                    snap,
                    _sde,
                    cid,
                    solar_system_id,
                    ship_id,
                    &hive_items,
                )
            });
            if let Some(container_id) = spawned_cid {
                let sys_handle = _systems.get_or_spawn(solar_system_id);
                let _ = sys_handle
                    .spawn_ball(make_site_npc_ball(container_id, 23, DVec3::new(10_500.0, 0.0, 0.0)))
                    .await;
            }
        }
    }

    // 3. Check Seasonal Event sites (single atomic snapshot transaction for expiry/window
    //    validation, objective progress, completion claim, cooldown arming, exploration/anomaly
    //    cleanup, #1032 tracking, and reward payout).
    enum SeasonalKillOutcome {
        PartialProgress,
        ExpiredCleanup {
            balls_to_remove: Vec<i64>,
        },
        Completed {
            acct_change: Option<(CharacterId, f64)>,
            relay_ball_to_remove: Option<i64>,
            container_info: Option<(i64, u32, [f64; 3], CharacterId)>,
        },
    }

    let seasonal_tx: Result<Option<SeasonalKillOutcome>, String> = db.try_with_mut_snapshot(|snap| {
        if snap.pve_special_sites.sim_clock_sec < now_sec {
            snap.pve_special_sites.sim_clock_sec = now_sec;
        }
        let mut matched_idx = None;
        if let Some(list) = snap.pve_special_sites.seasonal_event_sites.get(&solar_system_id) {
            for (idx, site) in list.iter().enumerate() {
                if !site.completed
                    && (site.boss_npc_ball_id == npc_ball_id
                        || site.escort_ball_ids.contains(&npc_ball_id))
                {
                    matched_idx = Some(idx);
                    break;
                }
            }
        }
        let Some(idx) = matched_idx else {
            return Ok(None);
        };

        let mut details_map = load_seasonal_site_details_map(snap);
        let active_season_id = active_season_in_snapshot(snap, now_ft).map(|s| s.season_id);
        let site_valid = {
            let site_ref = &snap
                .pve_special_sites
                .seasonal_event_sites
                .get(&solar_system_id)
                .ok_or_else(|| "missing seasonal site list".to_string())?[idx];
            seasonal_site_valid_in_snap(
                snap,
                solar_system_id,
                site_ref,
                active_season_id,
                now_ft,
                now_sec,
            )
        };
        if !site_valid {
            let stale_site = snap
                .pve_special_sites
                .seasonal_event_sites
                .get_mut(&solar_system_id)
                .ok_or_else(|| "missing seasonal site list".to_string())?
                .remove(idx);
            let mut balls_to_remove = vec![stale_site.boss_npc_ball_id];
            balls_to_remove.extend_from_slice(&stale_site.escort_ball_ids);
            if let Some(stale_details) = details_map.remove(&stale_site.instance_id) {
                if stale_details.relay_ball_id != 0 {
                    balls_to_remove.push(stale_details.relay_ball_id);
                    snap.transient_items.remove(&stale_details.relay_ball_id);
                    snap.item_positions.remove(&stale_details.relay_ball_id);
                }
                if !stale_details.exploration_site_id.is_empty() {
                    remove_seasonal_exploration_sites_in_snap(
                        snap,
                        solar_system_id,
                        now_sec,
                        &[stale_details.exploration_site_id],
                    )?;
                }
            }
            save_seasonal_site_details_map(snap, &details_map);
            let anom_key = format!("dng-{}-{}", stale_site.dungeon_id, stale_site.instance_id);
            if let Some(list) = snap.cosmic_anomalies.get_mut(&solar_system_id) {
                list.retain(|a| a.anomaly_id != anom_key);
            }
            crate::services::combat_crimewatch::with_mut_combat_runtime_in_snap(snap, |rt| {
                rt.bounty_credited_balls.insert(npc_ball_id);
                rt.record_destroyed_item_id(npc_ball_id);
                for &bid in &balls_to_remove {
                    if bid != npc_ball_id {
                        rt.npc_entities.remove(&bid);
                    }
                }
            });
            return Ok(Some(SeasonalKillOutcome::ExpiredCleanup { balls_to_remove }));
        }

        let site = &mut snap
            .pve_special_sites
            .seasonal_event_sites
            .get_mut(&solar_system_id)
            .ok_or_else(|| "missing seasonal site list".to_string())?[idx];
        let mut details = details_map.remove(&site.instance_id).unwrap_or_else(|| SeasonalSiteDetails {
            instance_id: site.instance_id,
            dungeon_id: site.dungeon_id,
            dungeon_name: site.dungeon_name.clone(),
            boss_type_id: site.boss_type_id,
            crate_type_id: site.loot_type_id,
            ..Default::default()
        });

        if site.boss_npc_ball_id == npc_ball_id {
            if details.boss_defeated {
                details_map.insert(site.instance_id, details);
                save_seasonal_site_details_map(snap, &details_map);
                return Ok(None);
            }
            details.boss_defeated = true;
        } else if site.escort_ball_ids.contains(&npc_ball_id) {
            if details.defeated_escort_ball_ids.contains(&npc_ball_id) {
                details_map.insert(site.instance_id, details);
                save_seasonal_site_details_map(snap, &details_map);
                return Ok(None);
            }
            details.defeated_escort_ball_ids.push(npc_ball_id);
        }

        let all_escorts_defeated = site
            .escort_ball_ids
            .iter()
            .all(|ebid| details.defeated_escort_ball_ids.contains(ebid));
        if !(details.boss_defeated && all_escorts_defeated) {
            details_map.insert(site.instance_id, details);
            save_seasonal_site_details_map(snap, &details_map);
            crate::services::combat_crimewatch::with_mut_combat_runtime_in_snap(snap, |rt| {
                rt.bounty_credited_balls.insert(npc_ball_id);
                rt.record_destroyed_item_id(npc_ball_id);
            });
            return Ok(Some(SeasonalKillOutcome::PartialProgress));
        }

        // Full objective completion: claim completion, arm per-system cooldown, clear registries,
        // and disburse all ISK, season points, crate, pool, and booster inside this transaction.
        let did = site.dungeon_id;
        let iid = site.instance_id;
        let dname = site.dungeon_name.clone();
        let reward_isk = site.reward_isk.max(0);
        let reward_points = site.reward_points.max(0);
        let loot_tid = site.loot_type_id;
        let site_position = details.site_position;
        site.completed = true;

        let mut cooldowns = load_seasonal_cooldown_store(snap);
        cooldowns.until_by_system.insert(
            solar_system_id,
            now_sec.saturating_add(SEASONAL_RESPAWN_COOLDOWN_SECS),
        );
        save_seasonal_cooldown_store(snap, &cooldowns);
        save_seasonal_site_details_map(snap, &details_map);
        crate::services::combat_crimewatch::with_mut_combat_runtime_in_snap(snap, |rt| {
            rt.bounty_credited_balls.insert(npc_ball_id);
            rt.record_destroyed_item_id(npc_ball_id);
        });

        if !details.exploration_site_id.is_empty() {
            let exp_id = &details.exploration_site_id;
            mutate_seasonal_exploration_reg_in_snap(snap, now_sec, |exp_reg| {
                let mut exp_changed = false;
                if let Some(sites) = exp_reg.sites_by_system.get_mut(&solar_system_id) {
                    for s in sites.iter_mut() {
                        if &s.site_id == exp_id && !s.cleared {
                            s.cleared = true;
                            exp_changed = true;
                        }
                    }
                }
                exp_changed
            })?;
        }

        let anom_key = format!("dng-{did}-{iid}");
        if let Some(list) = snap.cosmic_anomalies.get_mut(&solar_system_id) {
            for a in list.iter_mut() {
                if a.anomaly_id == anom_key {
                    a.cleared = true;
                }
            }
        }
        let relay_ball_to_remove = if details.relay_ball_id != 0 {
            snap.transient_items.remove(&details.relay_ball_id);
            snap.item_positions.remove(&details.relay_ball_id);
            Some(details.relay_ball_id)
        } else {
            None
        };

        let resolved_killer_cid = killer_char_id.and_then(|kid| {
            if snap.characters.contains_key(&kid) {
                Some(kid)
            } else {
                snap.characters
                    .values()
                    .find(|c| c.active_ship_id == kid)
                    .map(|c| c.character_id)
            }
        });

        let mut acct_change = None;
        let container_info = if let Some(cid) = resolved_killer_cid {
            let add_cents = IskCents::from_isk(reward_isk);
            let ch = snap
                .characters
                .get_mut(&cid)
                .ok_or_else(|| format!("missing killer character {cid}"))?;
            ch.balance = ch
                .balance
                .checked_add(add_cents)
                .ok_or_else(|| "seasonal completion ISK balance overflow".to_string())?;
            let new_bal = ch.balance;
            let ship_id = ch.active_ship_id;
            acct_change = Some((cid, new_bal.to_wire_f64()));
            snap.append_journal(
                cid,
                "BountyPrizes",
                add_cents,
                new_bal,
                format!("Seasonal Event Site Completion ({dname})"),
            );

            let mut st = crate::services::season_manager::load_season_state_from_blobs(snap, cid);
            st.season_points = st.season_points.saturating_add(reward_points);
            crate::services::season_manager::save_season_state_to_blobs(snap, cid, &st);

            let mut loot_specs = Vec::new();
            if loot_tid != 0 && _sde.types.contains_key(&loot_tid) {
                loot_specs.push((snap.alloc_entity_id(), loot_tid, 1));
            }
            if details.relay_hacked {
                for &(ptid, pqty) in &details.crate_loot_pool {
                    if ptid != loot_tid && pqty > 0 && _sde.types.contains_key(&ptid) {
                        let qty = pqty.clamp(1, i64::from(i32::MAX)) as i32;
                        loot_specs.push((snap.alloc_entity_id(), ptid, qty));
                    }
                }
            }
            let theme_hint = if details.season_theme.is_empty() {
                dname.as_str()
            } else {
                details.season_theme.as_str()
            };
            if let Some(booster_tid) = resolve_seasonal_booster_type_id(_sde, theme_hint) {
                if booster_tid != loot_tid {
                    loot_specs.push((snap.alloc_entity_id(), booster_tid, 1));
                }
            }
            let container_opt = deposit_loot_into_ship_or_container_in_snapshot(
                snap,
                _sde,
                cid,
                solar_system_id,
                ship_id,
                &loot_specs,
            );
            let cont_type = resolve_loot_container_type_id(_sde);
            let drop_pos = [
                site_position[0] + 500.0,
                site_position[1],
                site_position[2],
            ];
            if let Some(cid_container) = container_opt {
                if let Some(cont_item) = snap.get_any_item_mut(cid_container) {
                    cont_item.type_id = cont_type;
                    cont_item.flag = eve_db::InventoryFlag::None;
                }
                snap.item_positions.insert(cid_container, drop_pos);
                crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(
                    snap,
                    cid_container,
                    now_sec,
                    false,
                );
            }
            container_opt.map(|cid_container| (cid_container, cont_type, drop_pos, cid))
        } else {
            None
        };

        Ok(Some(SeasonalKillOutcome::Completed {
            acct_change,
            relay_ball_to_remove,
            container_info,
        }))
    });

    match seasonal_tx {
        Ok(Some(SeasonalKillOutcome::ExpiredCleanup { balls_to_remove })) => {
            let sys_handle = _systems.get_or_spawn(solar_system_id);
            for bid in balls_to_remove {
                sys_handle.try_remove_ball(bid);
            }
        }
        Ok(Some(SeasonalKillOutcome::Completed {
            acct_change,
            relay_ball_to_remove,
            container_info,
        })) => {
            if let Some(chg) = acct_change {
                acct_changes.push(chg);
            }
            let sys_handle = _systems.get_or_spawn(solar_system_id);
            if let Some(relay_bid) = relay_ball_to_remove {
                sys_handle.try_remove_ball(relay_bid);
            }
            if let Some((cid_container, cont_type, drop_pos, owner_cid)) = container_info {
                let (group_id, category_id, mass, radius, type_name) = _sde
                    .types
                    .get(&cont_type)
                    .map(|t| {
                        (
                            t.group_id,
                            t.category_id,
                            t.mass.max(100.0),
                            t.radius.max(10.0),
                            (!t.name.is_empty()).then(|| t.name.clone()),
                        )
                    })
                    .unwrap_or((12, 2, 1_000.0, 20.0, None));
                let cont_ball = eve_destiny::Ball {
                    id: cid_container,
                    type_id: cont_type,
                    position: DVec3::from_array(drop_pos),
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass,
                    radius,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                };
                let cont_meta = eve_destiny::SlimItemMeta {
                    group_id,
                    category_id,
                    owner_id: owner_cid,
                    char_id: None,
                    corp_id: -1,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false,
                    name: type_name,
                    ..Default::default()
                };
                let mut cont_cs =
                    crate::services::pve_exploration_seeding::build_type_combat_state_from_sde(
                        _sde,
                        cont_type,
                    );
                cont_cs.base_max_velocity = None;
                let _ = sys_handle
                    .spawn_ball_with_meta(cont_ball, cont_cs, cont_meta)
                    .await;
            }
        }
        Ok(Some(SeasonalKillOutcome::PartialProgress)) | Ok(None) => {}
        Err(err) => {
            tracing::warn!(
                solar_system_id,
                npc_ball_id,
                error = %err,
                "Seasonal site atomic completion transaction rolled back"
            );
        }
    }

    acct_changes
}

/// Live `hackingMgr` win callback for seasonal `SHK-*` Data relays.
/// Marks `relay_hacked = true` once per valid, non-expired seasonal site and weakens boss shield (~45%).
pub(crate) async fn on_seasonal_relay_hacked(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    site_id: &str,
    _char_id: CharacterId,
) {
    let now_ft = ctx.now_blueos_filetime_ticks();
    let now_sec = ctx.now_unix();
    let hack_target = ctx.db.with_mut_snapshot(|snap| {
        let season = active_season_in_snapshot(snap, now_ft)?;
        let mut map = load_seasonal_site_details_map(snap);
        let matched = map.iter().find_map(|(&iid, d)| {
            if d.exploration_site_id != site_id || d.relay_hacked {
                return None;
            }
            let site_rec = snap
                .pve_special_sites
                .seasonal_event_sites
                .get(&sys_id)
                .and_then(|list| {
                    list.iter().find(|s| {
                        s.instance_id == iid
                            && !s.completed
                            && s.season_id == season.season_id
                            && now_ft >= s.window_start_ft
                            && now_ft <= s.window_end_ft
                    })
                })?;
            let anom_id = format!("dng-{}-{}", site_rec.dungeon_id, site_rec.instance_id);
            let anom_valid = snap.cosmic_anomalies.get(&sys_id).is_some_and(|anoms| {
                anoms.iter().any(|a| {
                    a.anomaly_id == anom_id
                        && a.anomaly_type == "SeasonalEventSite"
                        && !a.cleared
                        && now_sec < a.created_unix.saturating_add(a.lifetime_seconds)
                })
            });
            anom_valid.then_some((iid, site_rec.boss_npc_ball_id, d.boss_defeated))
        });
        let (iid, boss_bid, boss_defeated) = matched?;
        let details = map.get_mut(&iid)?;
        details.relay_hacked = true;
        save_seasonal_site_details_map(snap, &map);
        Some((boss_bid, boss_defeated))
    });
    let Some((boss_bid, boss_defeated)) = hack_target else {
        return;
    };
    if boss_defeated {
        return;
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    if let Ok(Some(cs)) = sys_handle.get_combat_state(boss_bid).await {
        if cs.shield_hp > 20.0 {
            let drain_em = (cs.shield_hp * 0.45).min(cs.shield_hp - 1.0).max(0.0);
            let _ = sys_handle
                .apply_damage_with_attacker(
                    boss_bid,
                    eve_destiny::DamageVector::new(drain_em, 0.0, 0.0, 0.0),
                    None,
                )
                .await;
        }
    }
}

fn active_season_in_snapshot(
    snap: &eve_db::WorldStateSnapshot,
    now_ft: i64,
) -> Option<crate::services::season_fsd::SeasonConfig> {
    if let Some(cfg) = snap.seasons_and_campaigns.season_configuration.clone() {
        (now_ft >= cfg.start_time_ft && now_ft <= cfg.end_time_ft).then_some(cfg)
    } else if !snap.seasons_and_campaigns.seasons_catalog.is_empty() {
        snap.seasons_and_campaigns
            .seasons_catalog
            .values()
            .find(|s| now_ft >= s.start_time_ft && now_ft <= s.end_time_ft)
            .cloned()
    } else {
        None
    }
}

/// Strictly read-only view of active, uncompleted seasonal anomaly instances for `sys_id`.
/// Never mutates DB state and filters out completed, cleared, expired, or wrong-season entries.
pub(crate) fn active_seasonal_anomaly_instances_readonly(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> BTreeMap<u32, Vec<i64>> {
    let now_ft = ctx.now_blueos_filetime_ticks();
    let now_sec = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        let mut out: BTreeMap<u32, Vec<i64>> = BTreeMap::new();
        let Some(season) = active_season_in_snapshot(snap, now_ft) else {
            return out;
        };
        let Some(anoms) = snap.cosmic_anomalies.get(&sys_id) else {
            return out;
        };
        let sites = snap.pve_special_sites.seasonal_event_sites.get(&sys_id);
        for a in anoms {
            if a.anomaly_type != "SeasonalEventSite" || a.cleared {
                continue;
            }
            if now_sec >= a.created_unix.saturating_add(a.lifetime_seconds) {
                continue;
            }
            let Some(rest) = a.anomaly_id.strip_prefix("dng-") else {
                continue;
            };
            let Some((d_str, i_str)) = rest.split_once('-') else {
                continue;
            };
            let (Ok(did), Ok(iid)) = (d_str.parse::<u32>(), i_str.parse::<i64>()) else {
                continue;
            };
            let site_valid = sites.is_some_and(|list| {
                list.iter().any(|s| {
                    s.instance_id == iid
                        && !s.completed
                        && s.season_id == season.season_id
                        && now_ft >= s.window_start_ft
                        && now_ft <= s.window_end_ft
                })
            });
            if site_valid {
                out.entry(did).or_default().push(iid);
            }
        }
        out
    })
}

fn configure_seasonal_npc_entity(
    entity: &mut crate::services::combat_crimewatch::ExtendedNpcEntity,
    fallback_faction_id: u32,
) {
    entity.archetype = "Pirate".to_string();
    entity.bounty_isk = 0.0;
    if entity.faction_id == 0 {
        entity.faction_id = fallback_faction_id;
    }
    let total_dmg = entity.damage_vector.em
        + entity.damage_vector.thermal
        + entity.damage_vector.kinetic
        + entity.damage_vector.explosive;
    if total_dmg <= 0.0 {
        entity.damage_vector =
            crate::combat::npc_ai::faction_damage_vector(entity.faction_id, 100.0);
    }
    if entity.optimal_range < 15_000.0 {
        entity.optimal_range = 25_000.0;
    }
}

/// Spawns the stationary seasonal `SHK-*` data relay ball in `sys_handle`.
async fn spawn_seasonal_relay_ball(
    sys_handle: &crate::system_actor::SystemHandle,
    relay_bid: i64,
    relay_type_id: u32,
    relay_pos: DVec3,
) {
    let mut relay_ball = make_site_npc_ball(relay_bid, relay_type_id, relay_pos);
    relay_ball.max_velocity = 0.0;
    relay_ball.speed_fraction = 0.0;
    relay_ball.radius = 500.0;
    let _ = sys_handle.spawn_ball(relay_ball).await;
}

type SeasonalNpcBuild = (
    eve_destiny::Ball,
    eve_destiny::CombatState,
    eve_destiny::SlimItemMeta,
    crate::services::combat_crimewatch::ExtendedNpcEntity,
);

/// Builds the full Ballpark + AI components for a seasonal NPC without acquiring any DB locks.
fn build_seasonal_npc_full(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    ball_id: i64,
    type_id: u32,
    faction_id: u32,
    pos: DVec3,
) -> SeasonalNpcBuild {
    let archetype =
        crate::services::combat_crimewatch::combat_npc_ai::derive_npc_archetype(ctx, type_id);
    let (ball, cs, mut entity) =
        crate::services::wormhole_pve_seeding::build_specialized_npc_components(
            ctx,
            sys_id,
            ball_id,
            type_id,
            archetype,
            faction_id,
            pos,
        );
    configure_seasonal_npc_entity(&mut entity, faction_id);
    let sde_t = ctx.sde.types.get(&type_id);
    let fac_owner = if entity.faction_id > 0 {
        i64::from(entity.faction_id)
    } else {
        i64::from(faction_id.max(500_001))
    };
    let npc_meta = eve_destiny::SlimItemMeta {
        group_id: sde_t.map(|t| t.group_id).unwrap_or(546),
        category_id: sde_t.map(|t| t.category_id).unwrap_or(11),
        owner_id: fac_owner,
        char_id: None,
        corp_id: crate::services::combat_crimewatch::combat_npc_ai::resolve_npc_corp_id_for_faction(
            ctx, fac_owner,
        ),
        alliance_id: -1,
        security_status: -1.0,
        is_cloaked: false,
        name: sde_t.map(|t| t.name.clone()).filter(|n| !n.is_empty()),
        ..Default::default()
    };
    (ball, cs, npc_meta, entity)
}

fn make_seasonal_exploration_site(
    site_id: String,
    dungeon_id: u32,
    dungeon_name: String,
    sys_id: SolarSystemId,
    site_pos: [f64; 3],
    relay_bid: i64,
    created_unix: u64,
) -> crate::services::pve_exploration::PveSiteInstance {
    crate::services::pve_exploration::PveSiteInstance {
        site_id,
        dungeon_id,
        dungeon_name,
        solar_system_id: sys_id,
        site_category: "Data".into(),
        position: site_pos,
        probe_required: true,
        signal_strength: 1.0,
        current_room: 1,
        total_rooms: 1,
        current_wave: 1,
        total_waves: 1,
        hackable_can_ids: vec![relay_bid],
        npc_guards_spawned: true,
        created_unix,
        lifetime_seconds: 86_400,
        ..Default::default()
    }
}

async fn reconcile_post_spawn_seasonal_balls(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    sys_id: SolarSystemId,
    spawned_npcs: &[(i64, i64)],
    spawned_relays: &[(i64, i64)],
) -> Vec<crate::services::pve_special_sites::SeasonalEventSiteRecord> {
    let now_ft = ctx.now_blueos_filetime_ticks();
    let now_sec = ctx.now_unix();
    let (stale_balls, valid_sites) =
        crate::services::combat_crimewatch::with_mut_combat_runtime_tx_locks(
            &ctx.db,
            &*ctx.combat_ephemeral,
            |snap, rt| {
                let active_season_id = active_season_in_snapshot(snap, now_ft).map(|s| s.season_id);
                let details_map = load_seasonal_site_details_map(snap);
                let sites = snap.pve_special_sites.seasonal_event_sites.get(&sys_id);
                let mut stale = Vec::new();
                for &(iid, npc_bid) in spawned_npcs {
                    let npc_alive = sites
                        .and_then(|list| list.iter().find(|s| s.instance_id == iid))
                        .is_some_and(|s| {
                            seasonal_site_valid_in_snap(
                                snap,
                                sys_id,
                                s,
                                active_season_id,
                                now_ft,
                                now_sec,
                            ) && details_map.get(&iid).is_none_or(|d| {
                                if npc_bid == s.boss_npc_ball_id {
                                    !d.boss_defeated
                                } else {
                                    s.escort_ball_ids.contains(&npc_bid)
                                        && !d.defeated_escort_ball_ids.contains(&npc_bid)
                                }
                            })
                        });
                    if !npc_alive {
                        rt.npc_entities.remove(&npc_bid);
                        stale.push(npc_bid);
                    }
                }
                for &(iid, relay_bid) in spawned_relays {
                    let relay_valid = sites
                        .and_then(|list| list.iter().find(|s| s.instance_id == iid))
                        .is_some_and(|s| {
                            seasonal_site_valid_in_snap(
                                snap,
                                sys_id,
                                s,
                                active_season_id,
                                now_ft,
                                now_sec,
                            ) && details_map
                                .get(&iid)
                                .is_some_and(|d| d.relay_ball_id == relay_bid)
                        });
                    if !relay_valid {
                        stale.push(relay_bid);
                    }
                }
                let valid_sites = sites
                    .map(|list| {
                        list.iter()
                            .filter(|s| {
                                seasonal_site_valid_in_snap(
                                    snap,
                                    sys_id,
                                    s,
                                    active_season_id,
                                    now_ft,
                                    now_sec,
                                )
                            })
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                (stale, valid_sites)
            },
        );
    for bid in stale_balls {
        sys_handle.try_remove_ball(bid);
    }
    valid_sites
}

/// Spawns seasonal event sites in `sys_id` during an active seasonal event window (subject to
/// persisted per-system cooldown) and despawns completed, expired, or out-of-window ones.
pub async fn ensure_seasonal_event_sites_for_system(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> Vec<crate::services::pve_special_sites::SeasonalEventSiteRecord> {
    let sys_lock = seasonal_system_lock(&ctx.db, sys_id);
    let _sys_guard = sys_lock.lock().await;

    let now_ft = ctx.now_blueos_filetime_ticks();
    let now_sec = ctx.now_unix();

    let (active_season, expired_balls, expired_relay_balls, active_sites, details_by_iid, ai_present) =
        crate::services::combat_crimewatch::with_mut_combat_runtime_tx_locks(
            &ctx.db,
            &*ctx.combat_ephemeral,
            |snap, rt| {
                let active_season = active_season_in_snapshot(snap, now_ft);
                let active_season_id = active_season.as_ref().map(|a| a.season_id);

                if snap.pve_special_sites.sim_clock_sec < now_sec {
                    snap.pve_special_sites.sim_clock_sec = now_sec;
                }
                let mut active_anom_created: BTreeMap<String, u64> = BTreeMap::new();
                if let Some(anoms) = snap.cosmic_anomalies.get(&sys_id) {
                    for a in anoms {
                        if a.anomaly_type == "SeasonalEventSite"
                            && !a.cleared
                            && now_sec < a.created_unix.saturating_add(a.lifetime_seconds)
                        {
                            active_anom_created.insert(a.anomaly_id.clone(), a.created_unix);
                        }
                    }
                }

                let mut exp_ids = Vec::new();
                let mut exp_balls = Vec::new();
                let mut exp_anoms = Vec::new();
                let mut expired_relay_balls = Vec::new();
                let mut exp_sites = Vec::new();

                let prune_candidates: Vec<_> = snap
                    .pve_special_sites
                    .seasonal_event_sites
                    .get(&sys_id)
                    .into_iter()
                    .flatten()
                    .map(|s| {
                        let valid = seasonal_site_valid_in_snap(
                            snap,
                            sys_id,
                            s,
                            active_season_id,
                            now_ft,
                            now_sec,
                        );
                        (s.instance_id, valid)
                    })
                    .collect();
                if let Some(list) = snap.pve_special_sites.seasonal_event_sites.get_mut(&sys_id) {
                    list.retain(|s| {
                        let valid = prune_candidates
                            .iter()
                            .any(|&(iid, v)| iid == s.instance_id && v);
                        if !valid {
                            exp_ids.push(s.instance_id);
                            exp_balls.push(s.boss_npc_ball_id);
                            exp_balls.extend_from_slice(&s.escort_ball_ids);
                            exp_anoms.push(format!("dng-{}-{}", s.dungeon_id, s.instance_id));
                        }
                        valid
                    });
                }

                let mut details_map = load_seasonal_site_details_map(snap);
                if !exp_ids.is_empty() {
                    let mut details_changed = false;
                    for iid in &exp_ids {
                        if let Some(d) = details_map.remove(iid) {
                            details_changed = true;
                            if d.relay_ball_id != 0 {
                                expired_relay_balls.push(d.relay_ball_id);
                                snap.transient_items.remove(&d.relay_ball_id);
                                snap.item_positions.remove(&d.relay_ball_id);
                            }
                            if !d.exploration_site_id.is_empty() {
                                exp_sites.push(d.exploration_site_id);
                            }
                        }
                    }
                    if details_changed {
                        save_seasonal_site_details_map(snap, &details_map);
                    }
                }

                if !exp_anoms.is_empty() {
                    if let Some(list) = snap.cosmic_anomalies.get_mut(&sys_id) {
                        list.retain(|a| !exp_anoms.contains(&a.anomaly_id));
                    }
                }

                for bid in &exp_balls {
                    rt.npc_entities.remove(bid);
                }

                let active: Vec<_> = snap
                    .pve_special_sites
                    .seasonal_event_sites
                    .get(&sys_id)
                    .cloned()
                    .unwrap_or_default();

                let mut active_exp_restore = Vec::new();
                let mut ai_present = std::collections::BTreeSet::new();
                for site in &active {
                    let details_opt = details_map.get(&site.instance_id);
                    if let Some(d) = details_opt {
                        if d.relay_ball_id != 0 && d.relay_type_id != 0 {
                            if !snap.transient_items.contains_key(&d.relay_ball_id)
                                && !snap.items.contains_key(&d.relay_ball_id)
                            {
                                snap.insert_transient_item(eve_db::InventoryItemRecord {
                                    item_id: d.relay_ball_id,
                                    type_id: d.relay_type_id,
                                    owner_id: 1,
                                    location_id: i64::from(sys_id),
                                    flag: eve_db::InventoryFlag::None,
                                    quantity: 1,
                                    singleton: true,
                                    version: 1,
                                    ..Default::default()
                                });
                            }
                            snap.item_positions.entry(d.relay_ball_id).or_insert_with(|| {
                                [
                                    d.site_position[0],
                                    d.site_position[1] + 2_200.0,
                                    d.site_position[2],
                                ]
                            });
                            if !d.exploration_site_id.is_empty() {
                                let anom_id = format!("dng-{}-{}", site.dungeon_id, site.instance_id);
                                let created_unix = active_anom_created
                                    .get(&anom_id)
                                    .copied()
                                    .unwrap_or(now_sec);
                                active_exp_restore.push(make_seasonal_exploration_site(
                                    d.exploration_site_id.clone(),
                                    site.dungeon_id,
                                    site.dungeon_name.clone(),
                                    sys_id,
                                    d.site_position,
                                    d.relay_ball_id,
                                    created_unix,
                                ));
                            }
                        }
                        if d.boss_defeated {
                            rt.npc_entities.remove(&site.boss_npc_ball_id);
                        } else if rt.npc_entities.contains_key(&site.boss_npc_ball_id) {
                            ai_present.insert(site.boss_npc_ball_id);
                        }
                        for &ebid in &site.escort_ball_ids {
                            if d.defeated_escort_ball_ids.contains(&ebid) {
                                rt.npc_entities.remove(&ebid);
                            } else if rt.npc_entities.contains_key(&ebid) {
                                ai_present.insert(ebid);
                            }
                        }
                    } else {
                        if rt.npc_entities.contains_key(&site.boss_npc_ball_id) {
                            ai_present.insert(site.boss_npc_ball_id);
                        }
                        for &ebid in &site.escort_ball_ids {
                            if rt.npc_entities.contains_key(&ebid) {
                                ai_present.insert(ebid);
                            }
                        }
                    }
                }

                if let Err(err) = remove_seasonal_exploration_sites_in_snap(
                    snap,
                    sys_id,
                    now_sec,
                    &exp_sites,
                ) {
                    tracing::warn!(sys_id, error = %err, "Failed to prune expired exploration sites");
                }
                if !active_exp_restore.is_empty() {
                    if let Err(err) = mutate_seasonal_exploration_reg_in_snap(snap, now_sec, |reg| {
                        let mut changed = false;
                        let sites = reg.sites_by_system.entry(sys_id).or_default();
                        for entry in active_exp_restore {
                            if !sites.iter().any(|s| s.site_id == entry.site_id) {
                                sites.push(entry);
                                changed = true;
                            }
                        }
                        changed
                    }) {
                        tracing::warn!(sys_id, error = %err, "Failed to restore exploration sites during seasonal maintenance");
                    }
                }

                (
                    active_season,
                    exp_balls,
                    expired_relay_balls,
                    active,
                    details_map,
                    ai_present,
                )
            },
        );

    if !expired_balls.is_empty() || !expired_relay_balls.is_empty() {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        for bid in expired_balls.into_iter().chain(expired_relay_balls.into_iter()) {
            sys_handle.try_remove_ball(bid);
        }
    }

    // Re-materialize only living, non-defeated NPCs and missing relay balls for uncompleted sites
    // missing from `rt.npc_entities` or Ballpark (e.g. after server restart), re-validating
    // against live DB state before writing AI/Ballpark state so killed NPCs never resurrect.
    if !active_sites.is_empty() {
        let fallback_faction_id = active_season
            .as_ref()
            .map(|season| resolve_seasonal_event_spec(&ctx.sde, season, sys_id).faction_id)
            .unwrap_or(500_012);
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let mut missing_relays: Vec<(i64, i64, u32, DVec3)> = Vec::new();
        #[allow(clippy::type_complexity)]
        let mut missing_ballpark_npcs: Vec<(
            i64,
            i64,
            eve_destiny::Ball,
            eve_destiny::CombatState,
            eve_destiny::SlimItemMeta,
            Option<crate::services::combat_crimewatch::ExtendedNpcEntity>,
        )> = Vec::new();
        let mut missing_ai_only: Vec<(i64, i64, crate::services::combat_crimewatch::ExtendedNpcEntity)> = Vec::new();

        for site in &active_sites {
            let details = details_by_iid.get(&site.instance_id);
            let site_pos = DVec3::from_array(
                details
                    .map(|d| d.site_position)
                    .unwrap_or_else(default_seasonal_site_position),
            );
            let site_faction_id = details
                .map(|d| d.faction_id)
                .filter(|&fid| fid != 0)
                .unwrap_or(fallback_faction_id);
            if let Some(d) = details {
                if d.relay_ball_id != 0
                    && d.relay_type_id != 0
                    && sys_handle
                        .get_ball_snapshot(d.relay_ball_id)
                        .await
                        .ok()
                        .flatten()
                        .is_none()
                {
                    missing_relays.push((
                        site.instance_id,
                        d.relay_ball_id,
                        d.relay_type_id,
                        site_pos + DVec3::new(0.0, 2_200.0, 0.0),
                    ));
                }
            }
            let boss_defeated = details.is_some_and(|d| d.boss_defeated);
            let escort_tid = details.map(|d| d.escort_type_id).unwrap_or(site.boss_type_id);
            let mut candidates: Vec<(i64, u32, DVec3)> = Vec::new();
            if !boss_defeated {
                candidates.push((
                    site.boss_npc_ball_id,
                    site.boss_type_id,
                    site_pos + DVec3::new(2_500.0, 0.0, 0.0),
                ));
            }
            for (idx, &ebid) in site.escort_ball_ids.iter().enumerate() {
                if details.is_some_and(|d| d.defeated_escort_ball_ids.contains(&ebid)) {
                    continue;
                }
                let offset = match idx {
                    0 => DVec3::new(-2_500.0, 0.0, 0.0),
                    _ => DVec3::new(-3_200.0, 1_200.0, 0.0),
                };
                candidates.push((ebid, escort_tid, site_pos + offset));
            }
            for (bid, tid, default_pos) in candidates {
                let in_ai = ai_present.contains(&bid);
                match sys_handle.get_ball_snapshot(bid).await.ok().flatten() {
                    Some((_, existing_ball)) => {
                        if !in_ai {
                            let (_, _, _, entity) = build_seasonal_npc_full(
                                ctx,
                                sys_id,
                                bid,
                                tid,
                                site_faction_id,
                                existing_ball.position,
                            );
                            missing_ai_only.push((site.instance_id, bid, entity));
                        }
                    }
                    None => {
                        let (ball, cs, npc_meta, entity) = build_seasonal_npc_full(
                            ctx,
                            sys_id,
                            bid,
                            tid,
                            site_faction_id,
                            default_pos,
                        );
                        missing_ballpark_npcs.push((
                            site.instance_id,
                            bid,
                            ball,
                            cs,
                            npc_meta,
                            (!in_ai).then_some(entity),
                        ));
                    }
                }
            }
        }

        let now_ft_re = ctx.now_blueos_filetime_ticks();
        let now_sec_re = ctx.now_unix();
        let (surviving_relays, surviving_ballpark_npcs) =
            crate::services::combat_crimewatch::with_mut_combat_runtime_tx_locks(
                &ctx.db,
                &*ctx.combat_ephemeral,
                |snap, rt| {
                    let active_season_id =
                        active_season_in_snapshot(snap, now_ft_re).map(|s| s.season_id);
                    let details_map = load_seasonal_site_details_map(snap);
                    let sites = snap.pve_special_sites.seasonal_event_sites.get(&sys_id);
                    let is_site_live = |iid: i64| {
                        sites
                            .and_then(|list| list.iter().find(|s| s.instance_id == iid))
                            .is_some_and(|s| {
                                seasonal_site_valid_in_snap(
                                    snap,
                                    sys_id,
                                    s,
                                    active_season_id,
                                    now_ft_re,
                                    now_sec_re,
                                )
                            })
                    };
                    let is_npc_alive = |iid: i64, bid: i64| {
                        sites
                            .and_then(|list| list.iter().find(|s| s.instance_id == iid))
                            .is_some_and(|s| {
                                seasonal_site_valid_in_snap(
                                    snap,
                                    sys_id,
                                    s,
                                    active_season_id,
                                    now_ft_re,
                                    now_sec_re,
                                ) && details_map.get(&iid).is_none_or(|d| {
                                    if bid == s.boss_npc_ball_id {
                                        !d.boss_defeated
                                    } else {
                                        s.escort_ball_ids.contains(&bid)
                                            && !d.defeated_escort_ball_ids.contains(&bid)
                                    }
                                })
                            })
                    };

                    for (iid, bid, ent) in missing_ai_only {
                        if is_npc_alive(iid, bid) && !rt.npc_entities.contains_key(&bid) {
                            rt.npc_entities.insert(bid, ent);
                        }
                    }

                    let surviving_relays: Vec<_> = missing_relays
                        .into_iter()
                        .filter(|&(iid, relay_bid, _, _)| {
                            is_site_live(iid)
                                && details_map
                                    .get(&iid)
                                    .is_some_and(|d| d.relay_ball_id == relay_bid)
                        })
                        .collect();

                    let mut surviving_npcs = Vec::new();
                    for (iid, bid, ball, cs, npc_meta, ent_opt) in missing_ballpark_npcs {
                        if !is_npc_alive(iid, bid) {
                            continue;
                        }
                        if let Some(ent) = ent_opt {
                            if !rt.npc_entities.contains_key(&bid) {
                                rt.npc_entities.insert(bid, ent);
                            }
                        }
                        surviving_npcs.push((iid, bid, ball, cs, npc_meta));
                    }

                    (surviving_relays, surviving_npcs)
                },
            );

        let mut spawned_relay_checks = Vec::with_capacity(surviving_relays.len());
        for (iid, relay_bid, relay_tid, relay_pos) in surviving_relays {
            spawn_seasonal_relay_ball(&sys_handle, relay_bid, relay_tid, relay_pos).await;
            spawned_relay_checks.push((iid, relay_bid));
        }
        let mut spawned_npc_checks = Vec::with_capacity(surviving_ballpark_npcs.len());
        for (iid, bid, ball, cs, npc_meta) in surviving_ballpark_npcs {
            let _ = sys_handle.spawn_ball_with_meta(ball, cs, npc_meta).await;
            spawned_npc_checks.push((iid, bid));
        }
        return reconcile_post_spawn_seasonal_balls(
            ctx,
            &sys_handle,
            sys_id,
            &spawned_npc_checks,
            &spawned_relay_checks,
        )
        .await;
    }

    if let Some(season) = active_season {
        let cooldown_active = ctx.db.with_snapshot(|snap| {
            let cooldowns = load_seasonal_cooldown_store(snap);
            cooldowns
                .until_by_system
                .get(&sys_id)
                .copied()
                .is_some_and(|until| now_sec < until)
        });
        if cooldown_active {
            return active_sites;
        }

        let spec = resolve_seasonal_event_spec(&ctx.sde, &season, sys_id);

        enum SpawnReservation {
            Created {
                site: crate::services::pve_special_sites::SeasonalEventSiteRecord,
                relay_bid: i64,
                relay_pos: DVec3,
                npc_spawns: Vec<(eve_destiny::Ball, eve_destiny::CombatState, eve_destiny::SlimItemMeta)>,
            },
            AlreadyPresent(Vec<crate::services::pve_special_sites::SeasonalEventSiteRecord>),
            CooldownBlocked,
        }

        let reservation = crate::services::combat_crimewatch::with_mut_combat_runtime_tx_locks(
            &ctx.db,
            &*ctx.combat_ephemeral,
            |snap, rt| {
                if snap.pve_special_sites.sim_clock_sec < now_sec {
                    snap.pve_special_sites.sim_clock_sec = now_sec;
                }
                if let Some(existing) = snap.pve_special_sites.seasonal_event_sites.get(&sys_id) {
                    let active_existing: Vec<_> =
                        existing.iter().filter(|s| !s.completed).cloned().collect();
                    if !active_existing.is_empty() {
                        return SpawnReservation::AlreadyPresent(active_existing);
                    }
                }
                let mut cooldowns = load_seasonal_cooldown_store(snap);
                if cooldowns
                    .until_by_system
                    .get(&sys_id)
                    .copied()
                    .is_some_and(|until| now_sec < until)
                {
                    return SpawnReservation::CooldownBlocked;
                }
                let seq_entry = cooldowns.spawn_seq_by_system.entry(sys_id).or_default();
                *seq_entry += 1;
                let spawn_slot = *seq_entry;
                save_seasonal_cooldown_store(snap, &cooldowns);

                let au_scale = 0.35 + 0.15 * ((spawn_slot % 3) as f64);
                let pos_arr = crate::services::sov_fw_insurgency::derive_system_site_position_from_sde(
                    ctx,
                    sys_id,
                    spawn_slot,
                    au_scale,
                );
                let pos = DVec3::from_array(pos_arr);
                let relay_pos = pos + DVec3::new(0.0, 2_200.0, 0.0);

                snap.pve_special_sites.next_seq += 1;
                let inst_id = 1_895_000_000 + snap.pve_special_sites.next_seq;
                let boss_bid = snap.alloc_entity_id();
                let escort_bid_1 = snap.alloc_entity_id();
                let escort_bid_2 = snap.alloc_entity_id();
                let relay_bid = snap.alloc_entity_id();

                let rec = crate::services::pve_special_sites::SeasonalEventSiteRecord {
                    instance_id: inst_id,
                    season_id: season.season_id,
                    dungeon_id: spec.dungeon_id,
                    dungeon_name: spec.dungeon_name.clone(),
                    solar_system_id: sys_id,
                    window_start_ft: season.start_time_ft,
                    window_end_ft: season.end_time_ft,
                    boss_npc_ball_id: boss_bid,
                    boss_type_id: spec.boss_type_id,
                    escort_ball_ids: vec![escort_bid_1, escort_bid_2],
                    reward_isk: spec.reward_isk,
                    reward_points: spec.reward_points,
                    loot_type_id: spec.crate_type_id,
                    completed: false,
                };
                snap.pve_special_sites
                    .seasonal_event_sites
                    .entry(sys_id)
                    .or_default()
                    .push(rec.clone());

                let exp_site_id = format!("SHK-{}-{}", rec.dungeon_id, rec.instance_id);
                let details = SeasonalSiteDetails {
                    instance_id: rec.instance_id,
                    season_theme: spec.theme.label().to_string(),
                    dungeon_id: rec.dungeon_id,
                    dungeon_name: rec.dungeon_name.clone(),
                    faction_id: spec.faction_id,
                    boss_type_id: rec.boss_type_id,
                    escort_type_id: spec.escort_type_id,
                    relay_ball_id: relay_bid,
                    relay_type_id: spec.relay_type_id,
                    relay_hacked: false,
                    exploration_site_id: exp_site_id.clone(),
                    crate_type_id: spec.crate_type_id,
                    crate_loot_pool: spec.crate_loot_pool.clone(),
                    boss_defeated: false,
                    defeated_escort_ball_ids: Vec::new(),
                    site_position: pos_arr,
                };
                let mut details_map = load_seasonal_site_details_map(snap);
                details_map.insert(rec.instance_id, details);
                save_seasonal_site_details_map(snap, &details_map);

                snap.insert_transient_item(eve_db::InventoryItemRecord {
                    item_id: relay_bid,
                    type_id: spec.relay_type_id,
                    owner_id: 1,
                    location_id: i64::from(sys_id),
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                });
                snap.item_positions
                    .insert(relay_bid, [relay_pos.x, relay_pos.y, relay_pos.z]);

                snap.cosmic_anomalies
                    .entry(sys_id)
                    .or_default()
                    .push(eve_db::CosmicAnomalyRecord {
                        anomaly_id: format!("dng-{}-{}", rec.dungeon_id, rec.instance_id),
                        solar_system_id: sys_id,
                        anomaly_type: "SeasonalEventSite".to_string(),
                        name: rec.dungeon_name.clone(),
                        position: pos_arr,
                        signal_strength: 1.0,
                        ore_types: Vec::new(),
                        remaining_volume: 0.0,
                        created_unix: now_sec,
                        lifetime_seconds: 86_400,
                        cleared: false,
                    });

                let exp_entry = make_seasonal_exploration_site(
                    exp_site_id,
                    rec.dungeon_id,
                    rec.dungeon_name.clone(),
                    sys_id,
                    pos_arr,
                    relay_bid,
                    now_sec,
                );
                if let Err(err) = mutate_seasonal_exploration_reg_in_snap(snap, now_sec, |reg| {
                    let sites = reg.sites_by_system.entry(sys_id).or_default();
                    if !sites.iter().any(|s| s.site_id == exp_entry.site_id) {
                        sites.push(exp_entry);
                        true
                    } else {
                        false
                    }
                }) {
                    tracing::warn!(sys_id, error = %err, "Failed to register seasonal exploration site");
                }

                let mut npc_spawns = Vec::with_capacity(3);
                for (bid, tid, npc_pos) in [
                    (
                        rec.boss_npc_ball_id,
                        rec.boss_type_id,
                        pos + DVec3::new(2_500.0, 0.0, 0.0),
                    ),
                    (
                        rec.escort_ball_ids[0],
                        spec.escort_type_id,
                        pos + DVec3::new(-2_500.0, 0.0, 0.0),
                    ),
                    (
                        rec.escort_ball_ids[1],
                        spec.escort_type_id,
                        pos + DVec3::new(-3_200.0, 1_200.0, 0.0),
                    ),
                ] {
                    let (ball, cs, npc_meta, entity) = build_seasonal_npc_full(
                        ctx,
                        sys_id,
                        bid,
                        tid,
                        spec.faction_id,
                        npc_pos,
                    );
                    rt.npc_entities.insert(bid, entity);
                    npc_spawns.push((ball, cs, npc_meta));
                }

                SpawnReservation::Created {
                    site: rec,
                    relay_bid,
                    relay_pos,
                    npc_spawns,
                }
            },
        );

        match reservation {
            SpawnReservation::AlreadyPresent(existing) => return existing,
            SpawnReservation::CooldownBlocked => return active_sites,
            SpawnReservation::Created {
                site: new_site,
                relay_bid,
                relay_pos,
                npc_spawns,
            } => {
                let sys_handle = ctx.systems.get_or_spawn(sys_id);
                let check_npcs: Vec<(i64, i64)> = npc_spawns
                    .iter()
                    .map(|(ball, _, _)| (new_site.instance_id, ball.id))
                    .collect();
                let check_relays = [(new_site.instance_id, relay_bid)];
                for (ball, cs, npc_meta) in npc_spawns {
                    let _ = sys_handle.spawn_ball_with_meta(ball, cs, npc_meta).await;
                }
                spawn_seasonal_relay_ball(&sys_handle, relay_bid, spec.relay_type_id, relay_pos)
                    .await;
                return reconcile_post_spawn_seasonal_balls(
                    ctx,
                    &sys_handle,
                    sys_id,
                    &check_npcs,
                    &check_relays,
                )
                .await;
            }
        }
    }

    active_sites
}

#[cfg(test)]
#[path = "pve_special_sites_combat_tests.rs"]
mod pve_special_sites_combat_tests;
#[cfg(test)]
#[path = "pve_special_sites_combat_edges_tests.rs"]
mod pve_special_sites_combat_edges_tests;

