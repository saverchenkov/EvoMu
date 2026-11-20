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
use std::collections::BTreeMap;
use std::sync::Arc;

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
    let mut preferred: Option<u32> = None;
    let mut fallback: Option<u32> = None;
    for t in sde.types.values() {
        if !is_valid_booster(t) {
            continue;
        }
        fallback = Some(fallback.map_or(t.type_id, |prev| prev.min(t.type_id)));
        let lower = t.name.to_lowercase();
        if theme_terms.iter().any(|kw| lower.contains(kw))
            || lower.contains("pyrolancea")
            || lower.contains("blue pill")
        {
            preferred = Some(preferred.map_or(t.type_id, |prev| prev.min(t.type_id)));
        }
    }
    preferred.or(fallback)
}

fn resolve_loot_container_type_id(sde: &eve_sde::SdeRegistry) -> u32 {
    if let Some(tid) = sde
        .types
        .values()
        .filter(|t| {
            t.published
                && t.group_id == 12
                && t.category_id == 2
                && t.name.to_lowercase().contains("cargo container")
        })
        .map(|t| t.type_id)
        .min()
    {
        return tid;
    }
    first_known_type(sde, &[17_996, 23]).unwrap_or(23)
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

    let sec = sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.5);
    let sec_mult = if sec >= 0.45 {
        1.0
    } else if sec > 0.0 {
        1.5
    } else {
        2.0
    };
    let difficulty = dungeon_difficulty
        .unwrap_or(match dungeon_archetype {
            Some(31) => 4,
            Some(45) => 2,
            _ => 3,
        })
        .clamp(1, 5);
    let base_isk = 1_500_000 + i64::from(difficulty) * 250_000;
    let reward_isk = ((base_isk as f64) * sec_mult).round() as i64;
    let faction_id = dungeon_faction_id.unwrap_or(match theme {
        SeasonTheme::CrimsonHarvest => 500_012,
        SeasonTheme::WinterNexus => 500_019,
        SeasonTheme::GuardiansGala => 500_020,
        SeasonTheme::LiberationDay => 500_002,
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

    // 3. Check Seasonal Event sites (single atomic snapshot transaction for objective progress,
    //    completion claim, cooldown arming, exploration/anomaly cleanup, and reward payout).
    enum SeasonalKillOutcome {
        PartialProgress,
        Completed {
            acct_change: Option<(CharacterId, f64)>,
            relay_ball_to_remove: Option<i64>,
            container_id: Option<i64>,
            site_position: [f64; 3],
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

        if !details.exploration_site_id.is_empty() {
            if let Some(note) = snap
                .owner_notes
                .get_mut(&crate::services::pve_exploration::PVE_EXPLORATION_NOTE_ID)
            {
                let mut exp_reg =
                    serde_json::from_str::<crate::services::pve_exploration::PveExplorationRegistry>(
                        &note.note,
                    )
                    .map_err(|e| format!("failed to parse exploration registry: {e}"))?;
                let mut exp_changed = false;
                if let Some(sites) = exp_reg.sites_by_system.get_mut(&solar_system_id) {
                    for s in sites.iter_mut() {
                        if s.site_id == details.exploration_site_id && !s.cleared {
                            s.cleared = true;
                            exp_changed = true;
                        }
                    }
                }
                if exp_changed {
                    note.note = serde_json::to_string(&exp_reg)
                        .map_err(|e| format!("failed to serialize exploration registry: {e}"))?;
                    note.timestamp_unix = now_sec;
                }
            }
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

        let mut acct_change = None;
        let container_id = if let Some(cid) = killer_char_id {
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
            deposit_loot_into_ship_or_container_in_snapshot(
                snap,
                _sde,
                cid,
                solar_system_id,
                ship_id,
                &loot_specs,
            )
        } else {
            None
        };

        Ok(Some(SeasonalKillOutcome::Completed {
            acct_change,
            relay_ball_to_remove,
            container_id,
            site_position,
        }))
    });

    match seasonal_tx {
        Ok(Some(SeasonalKillOutcome::Completed {
            acct_change,
            relay_ball_to_remove,
            container_id,
            site_position,
        })) => {
            if let Some(chg) = acct_change {
                acct_changes.push(chg);
            }
            let sys_handle = _systems.get_or_spawn(solar_system_id);
            if let Some(relay_bid) = relay_ball_to_remove {
                sys_handle.try_remove_ball(relay_bid);
            }
            if let Some(cid_container) = container_id {
                let cont_type = resolve_loot_container_type_id(_sde);
                let drop_pos = DVec3::from_array(site_position) + DVec3::new(500.0, 0.0, 0.0);
                let _ = sys_handle
                    .spawn_ball(make_site_npc_ball(cid_container, cont_type, drop_pos))
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
/// Marks `relay_hacked = true` so bonus FSD `crate_loot_pool` drops and weakens boss shield (~45%).
pub(crate) async fn on_seasonal_relay_hacked(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    site_id: &str,
    _char_id: CharacterId,
) {
    let inst_id_opt = ctx.db.with_mut_snapshot(|snap| {
        let mut map = load_seasonal_site_details_map(snap);
        let found_iid = map
            .iter_mut()
            .find(|(_, d)| d.exploration_site_id == site_id && !d.boss_defeated)
            .map(|(&iid, d)| {
                d.relay_hacked = true;
                iid
            });
        if found_iid.is_some() {
            save_seasonal_site_details_map(snap, &map);
        }
        found_iid
    });
    let Some(inst_id) = inst_id_opt else {
        return;
    };
    let boss_bid_opt = with_pve_special_reg(ctx, |reg| {
        reg.seasonal_event_sites
            .get(&sys_id)
            .and_then(|list| {
                list.iter()
                    .find(|s| s.instance_id == inst_id && !s.completed)
                    .map(|s| s.boss_npc_ball_id)
            })
    });
    if let Some(boss_bid) = boss_bid_opt {
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

/// Spawns seasonal event sites in `sys_id` during an active seasonal event window (subject to
/// persisted per-system cooldown) and despawns completed, expired, or out-of-window ones.
pub async fn ensure_seasonal_event_sites_for_system(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> Vec<crate::services::pve_special_sites::SeasonalEventSiteRecord> {
    let now_ft = ctx.now_blueos_filetime_ticks();
    let now_sec = ctx.now_unix();
    let active_season = ctx.db.with_snapshot(|snap| active_season_in_snapshot(snap, now_ft));
    let in_window = active_season.is_some();
    let active_season_id = active_season.as_ref().map(|a| a.season_id);

    let (expired_ids, expired_balls, expired_anom_ids, mut active_sites) =
        with_pve_special_reg(ctx, |reg| {
            let mut exp_ids = Vec::new();
            let mut exp_balls = Vec::new();
            let mut exp_anoms = Vec::new();
            if let Some(list) = reg.seasonal_event_sites.get_mut(&sys_id) {
                list.retain(|s| {
                    let valid = in_window
                        && !s.completed
                        && active_season_id.is_some_and(|sid| s.season_id == sid)
                        && now_ft >= s.window_start_ft
                        && now_ft <= s.window_end_ft;
                    if !valid {
                        exp_ids.push(s.instance_id);
                        exp_balls.push(s.boss_npc_ball_id);
                        exp_balls.extend_from_slice(&s.escort_ball_ids);
                        exp_anoms.push(format!("dng-{}-{}", s.dungeon_id, s.instance_id));
                    }
                    valid
                });
            }
            let active = reg
                .seasonal_event_sites
                .get(&sys_id)
                .cloned()
                .unwrap_or_default();
            (exp_ids, exp_balls, exp_anoms, active)
        });

    if !expired_ids.is_empty() || !expired_balls.is_empty() || !expired_anom_ids.is_empty() {
        let mut relay_balls = Vec::new();
        let mut exp_sites = Vec::new();
        ctx.db.with_mut_snapshot(|snap| {
            let mut details_map = load_seasonal_site_details_map(snap);
            for iid in &expired_ids {
                if let Some(d) = details_map.remove(iid) {
                    if d.relay_ball_id != 0 {
                        relay_balls.push(d.relay_ball_id);
                        snap.transient_items.remove(&d.relay_ball_id);
                        snap.item_positions.remove(&d.relay_ball_id);
                    }
                    if !d.exploration_site_id.is_empty() {
                        exp_sites.push(d.exploration_site_id);
                    }
                }
            }
            save_seasonal_site_details_map(snap, &details_map);
            if let Some(list) = snap.cosmic_anomalies.get_mut(&sys_id) {
                list.retain(|a| !expired_anom_ids.contains(&a.anomaly_id));
            }
        });
        if !expired_balls.is_empty() {
            crate::services::combat_crimewatch::with_mut_combat_ephemeral(ctx, |rt| {
                for bid in &expired_balls {
                    rt.npc_entities.remove(bid);
                }
            });
        }
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        for bid in expired_balls.into_iter().chain(relay_balls.into_iter()) {
            sys_handle.try_remove_ball(bid);
        }
        if !exp_sites.is_empty() {
            crate::services::pve_exploration::with_pve_exploration_reg(ctx, |reg| {
                if let Some(sites) = reg.sites_by_system.get_mut(&sys_id) {
                    sites.retain(|s| !exp_sites.contains(&s.site_id));
                }
            });
        }
    }

    // Re-materialize only living, non-defeated NPCs for any existing uncompleted site missing from
    // `rt.npc_entities` or ballpark (e.g. after server restart), never resurrecting defeated ones.
    if !active_sites.is_empty() {
        let details_by_iid = ctx.db.with_snapshot(load_seasonal_site_details_map);
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let mut rebuilt = Vec::new();
        for site in &active_sites {
            let details = details_by_iid.get(&site.instance_id);
            let site_pos = DVec3::from_array(
                details
                    .map(|d| d.site_position)
                    .unwrap_or_else(default_seasonal_site_position),
            );
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
            for (bid, tid, npc_pos) in candidates {
                let in_ai = crate::services::combat_crimewatch::with_combat_ephemeral(ctx, |rt| {
                    rt.npc_entities.contains_key(&bid)
                });
                let in_ballpark = sys_handle.get_ball_snapshot(bid).await.ok().flatten().is_some();
                if !in_ai || !in_ballpark {
                    let _ = crate::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_batch_with_start_id(
                        ctx,
                        sys_id,
                        &[(tid, "", 0, npc_pos)],
                        bid,
                        |entity| configure_seasonal_npc_entity(entity, 500_012),
                        &mut rebuilt,
                    )
                    .await;
                }
            }
        }
        if !rebuilt.is_empty() {
            crate::services::combat_crimewatch::with_mut_combat_ephemeral(ctx, |rt| {
                for (bid, ent) in rebuilt {
                    rt.npc_entities.insert(bid, ent);
                }
            });
        }
        return active_sites;
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
        let spawn_slot = ctx.db.with_snapshot(|snap| {
            load_seasonal_cooldown_store(snap)
                .spawn_seq_by_system
                .get(&sys_id)
                .copied()
                .unwrap_or(0)
                + 1
        });
        let au_scale = 0.35 + 0.15 * ((spawn_slot % 3) as f64);
        let pos_arr = crate::services::sov_fw_insurgency::derive_system_site_position_from_sde(
            ctx,
            sys_id,
            spawn_slot,
            au_scale,
        );
        let pos = DVec3::from_array(pos_arr);
        let relay_pos = pos + DVec3::new(0.0, 2_200.0, 0.0);

        enum SpawnReservation {
            Created(crate::services::pve_special_sites::SeasonalEventSiteRecord, i64, String),
            AlreadyPresent(Vec<crate::services::pve_special_sites::SeasonalEventSiteRecord>),
            CooldownBlocked,
        }

        let reservation = ctx.db.with_mut_snapshot(|snap| {
            if snap.pve_special_sites.sim_clock_sec < now_sec {
                snap.pve_special_sites.sim_clock_sec = now_sec;
            }
            if let Some(existing) = snap.pve_special_sites.seasonal_event_sites.get(&sys_id) {
                if !existing.is_empty() {
                    return SpawnReservation::AlreadyPresent(existing.clone());
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
            save_seasonal_cooldown_store(snap, &cooldowns);

            snap.pve_special_sites.next_seq += 1;
            let inst_id = 1_895_000_000 + snap.pve_special_sites.next_seq;
            let npc_start_id = snap.next_entity_id + 1;
            snap.next_entity_id += 3;
            let boss_bid = npc_start_id;
            let escort_bid_1 = npc_start_id + 1;
            let escort_bid_2 = npc_start_id + 2;
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

            SpawnReservation::Created(rec, relay_bid, exp_site_id)
        });

        match reservation {
            SpawnReservation::AlreadyPresent(existing) => return existing,
            SpawnReservation::CooldownBlocked => return active_sites,
            SpawnReservation::Created(new_site, relay_bid, exp_site_id) => {
                crate::services::pve_exploration::with_pve_exploration_reg(ctx, |reg| {
                    reg.sites_by_system
                        .entry(sys_id)
                        .or_default()
                        .push(crate::services::pve_exploration::PveSiteInstance {
                            site_id: exp_site_id,
                            dungeon_id: new_site.dungeon_id,
                            dungeon_name: new_site.dungeon_name.clone(),
                            solar_system_id: sys_id,
                            site_category: "Data".into(),
                            position: pos_arr,
                            probe_required: true,
                            signal_strength: 1.0,
                            current_room: 1,
                            total_rooms: 1,
                            current_wave: 1,
                            total_waves: 1,
                            hackable_can_ids: vec![relay_bid],
                            npc_guards_spawned: true,
                            created_unix: now_sec,
                            lifetime_seconds: 86_400,
                            ..Default::default()
                        });
                });

                let npc_specs = [
                    (
                        new_site.boss_type_id,
                        "",
                        spec.faction_id,
                        pos + DVec3::new(2_500.0, 0.0, 0.0),
                    ),
                    (
                        spec.escort_type_id,
                        "",
                        spec.faction_id,
                        pos + DVec3::new(-2_500.0, 0.0, 0.0),
                    ),
                    (
                        spec.escort_type_id,
                        "",
                        spec.faction_id,
                        pos + DVec3::new(-3_200.0, 1_200.0, 0.0),
                    ),
                ];
                let mut built = Vec::with_capacity(3);
                let _ = crate::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_batch_with_start_id(
                    ctx,
                    sys_id,
                    &npc_specs,
                    new_site.boss_npc_ball_id,
                    |entity| configure_seasonal_npc_entity(entity, spec.faction_id),
                    &mut built,
                )
                .await;
                crate::services::combat_crimewatch::with_mut_combat_ephemeral(ctx, |rt| {
                    for (bid, entity) in built {
                        rt.npc_entities.insert(bid, entity);
                    }
                });

                let sys_handle = ctx.systems.get_or_spawn(sys_id);
                let mut relay_ball = make_site_npc_ball(relay_bid, spec.relay_type_id, relay_pos);
                relay_ball.max_velocity = 0.0;
                relay_ball.speed_fraction = 0.0;
                relay_ball.radius = 500.0;
                let _ = sys_handle.spawn_ball(relay_ball).await;

                active_sites.push(new_site);
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

