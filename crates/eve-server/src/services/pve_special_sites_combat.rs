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
        if lower.contains("winter") || season.season_id == 2 {
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

    fn dungeon_candidates(self) -> &'static [u32] {
        match self {
            Self::CrimsonHarvest => &[8_923, 8_926, 9_238, 9_241, 8_933],
            Self::WinterNexus => &[9_030, 9_046, 9_047, 9_029],
            Self::GuardiansGala => &[8_678, 9_051],
            Self::LiberationDay => &[9_180, 9_207],
        }
    }

    fn crate_candidates(self) -> &'static [u32] {
        match self {
            Self::CrimsonHarvest => &[
                53_060, 53_055, 53_057, 53_058, 97_206, 90_134, 90_135, 90_137,
            ],
            Self::WinterNexus => &[86_495, 86_496, 90_739],
            Self::GuardiansGala => &[50_045],
            Self::LiberationDay => &[95_657, 55_807],
        }
    }

    // DERIVED: V24 SDE seasonal dungeons encode item crate drops (`crates.fsdbinary`) and
    // season point thresholds (`fsd_challenge_definitions`), while per-site ISK completion
    // bounties are synthesized per seasonal theme and security multiplier.
    fn base_isk(self) -> i64 {
        match self {
            Self::CrimsonHarvest => 2_250_000,
            Self::WinterNexus => 2_500_000,
            Self::GuardiansGala => 3_000_000,
            Self::LiberationDay => 2_000_000,
        }
    }
}

struct ResolvedSeasonalSpec {
    theme: SeasonTheme,
    dungeon_id: u32,
    dungeon_name: String,
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

// DERIVED: Client FSD provides `season.fsdbinary`, `crates.fsdbinary`, `challenges.fsdbinary`, and
// `sde_dungeons` header entries (`archetype_id == 45`), but omits CCP's per-site NPC ship picks,
// wave/relay pairings, and security-status ISK scaling; those ship/relay candidate lists and sec
// multipliers are server-derived, while crate loot tables and challenge points come from FSD.
fn resolve_seasonal_event_spec(
    sde: &eve_sde::SdeRegistry,
    season: &eve_db::SeasonConfig,
    sys_id: SolarSystemId,
) -> ResolvedSeasonalSpec {
    let theme = SeasonTheme::from_season(season);
    let mut matching_dungeons: Vec<(u32, String)> = theme
        .dungeon_candidates()
        .iter()
        .filter_map(|did| {
            sde.dungeons
                .get(did)
                .map(|d| (d.dungeon_id, d.name.trim().to_string()))
        })
        .collect();
    if matching_dungeons.is_empty() {
        matching_dungeons = sde
            .dungeons
            .values()
            .filter(|d| d.archetype_id == Some(45))
            .map(|d| (d.dungeon_id, d.name.trim().to_string()))
            .collect();
        matching_dungeons.sort_by_key(|(did, _)| *did);
    }

    let (dungeon_id, dungeon_name) = if !matching_dungeons.is_empty() {
        let pick_idx = (sys_id as usize + season.season_id.unsigned_abs() as usize)
            % matching_dungeons.len();
        matching_dungeons[pick_idx].clone()
    } else {
        (8_923, "Tetrimon Base".to_string())
    };

    let lower_dname = dungeon_name.to_lowercase();
    let (boss_candidates, escort_candidates, relay_candidates): (&[u32], &[u32], &[u32]) =
        match theme {
            SeasonTheme::CrimsonHarvest if lower_dname.contains("tetrimon") => (
                &[56_676, 89_489, 59_990, 56_648],
                &[56_648, 59_990],
                &[89_515, 60_712, 59_503, 23],
            ),
            SeasonTheme::CrimsonHarvest => (
                &[17_615, 56_676, 17_993],
                &[17_993, 56_648],
                &[89_516, 60_710, 59_503, 23],
            ),
            SeasonTheme::WinterNexus => (
                &[56_871, 24_195],
                &[56_871, 24_191],
                &[91_044, 59_503, 23],
            ),
            SeasonTheme::GuardiansGala => (
                &[57_321, 53_947],
                &[53_947, 57_321],
                &[59_503, 23],
            ),
            SeasonTheme::LiberationDay => (
                &[56_676, 17_615],
                &[56_648, 17_993],
                &[59_503, 23],
            ),
        };

    let boss_type_id = first_known_type(sde, boss_candidates)
        .or_else(|| {
            sde.types
                .values()
                .find(|t| t.published && matches!(t.group_id, 550 | 552 | 553))
                .map(|t| t.type_id)
        })
        .unwrap_or(56_676);
    let escort_type_id = first_known_type(sde, escort_candidates)
        .or_else(|| {
            sde.types
                .values()
                .find(|t| t.published && matches!(t.group_id, 550 | 551))
                .map(|t| t.type_id)
        })
        .unwrap_or(boss_type_id);
    let relay_type_id = first_known_type(sde, relay_candidates).unwrap_or(23);

    // Avoid poisoning OnceLock tables inside `season_fsd` when synthetic unit-test SDEs omit full FSD data.
    let (crate_type_id, crate_loot_pool, reward_points) = if sde.types.len() > 100 {
        let crate_tables = crate::services::season_fsd::fsd_crate_loot_tables(sde);
        let picked_crate = theme
            .crate_candidates()
            .iter()
            .copied()
            .find(|tid| crate_tables.contains_key(tid))
            .or_else(|| crate_tables.keys().copied().next())
            .or_else(|| first_known_type(sde, theme.crate_candidates()))
            .unwrap_or(53_060);
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
        let picked_crate = first_known_type(sde, theme.crate_candidates())
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
    let reward_isk = ((theme.base_isk() as f64) * sec_mult).round() as i64;

    ResolvedSeasonalSpec {
        theme,
        dungeon_id,
        dungeon_name,
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

    // 0. Check Homefront sites when destroyed with attacker attribution in space
    if let Some(cid) = killer_char_id {
        let fleet_pilots: Vec<CharacterId> = db.with_snapshot(|snap| {
            let fleet_id = snap.characters.get(&cid).and_then(|c| c.fleet_id);
            if let Some(fid) = fleet_id {
                let mut members: Vec<CharacterId> = snap
                    .characters
                    .values()
                    .filter(|c| {
                        c.fleet_id == Some(fid)
                            && c.solar_system_id == solar_system_id
                            && c.station_id.is_none()
                            && c.structure_id.is_none()
                    })
                    .map(|c| c.character_id)
                    .collect();
                if !members.contains(&cid) {
                    members.push(cid);
                }
                members.sort_unstable();
                members
            } else {
                vec![cid]
            }
        });
        let fleet_count = fleet_pilots.len().max(1);
        let hf_res = with_pve_special_reg_db(db, now_sec, |reg| {
            if let Some(list) = reg.homefront_sites.get_mut(&solar_system_id) {
                for site in list.iter_mut() {
                    if site.boss_npc_ball_id == npc_ball_id && !site.completed {
                        let reward_paid =
                            crate::services::pve_special_sites_ops::compute_homefront_combat_pilot_isk(
                                site.base_pilot_isk,
                                site.optimal_fleet_size,
                                fleet_count,
                            );
                        site.completed = true;
                        site.reward_paid_isk = reward_paid;
                        return Some((site.dungeon_name.clone(), reward_paid));
                    }
                }
            }
            None
        });
        if let Some((dungeon_name, reward_paid)) = hf_res {
            db.with_mut_snapshot(|snap| {
                for &pid in &fleet_pilots {
                    if let Some(ch) = snap.characters.get_mut(&pid) {
                        ch.balance = ch
                            .balance
                            .checked_add(IskCents::from_isk(reward_paid))
                            .unwrap_or(ch.balance);
                        let new_bal = ch.balance;
                        acct_changes.push((pid, new_bal.to_wire_f64()));
                        snap.append_journal(
                            pid,
                            "CorporateRewardPayout",
                            IskCents::from_isk(reward_paid),
                            new_bal,
                            format!("Homefront Operation Payout ({dungeon_name})"),
                        );
                    }
                }
            });
            return acct_changes;
        }
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

    // 3. Check Seasonal Event sites
    let mut seasonal_escorts = Vec::new();
    let seasonal_res = with_pve_special_reg_db(db, now_sec, |reg| {
        let mut found = None;
        if let Some(list) = reg.seasonal_event_sites.get_mut(&solar_system_id) {
            for site in list.iter_mut() {
                if site.boss_npc_ball_id == npc_ball_id && !site.completed {
                    site.completed = true;
                    seasonal_escorts = site.escort_ball_ids.clone();
                    found = Some((
                        site.dungeon_id,
                        site.instance_id,
                        site.dungeon_name.clone(),
                        site.reward_isk,
                        site.reward_points,
                        site.loot_type_id,
                    ));
                    break;
                }
            }
        }
        found
    });

    if let Some((did, iid, dname, reward_isk, reward_points, loot_tid)) = seasonal_res {
        let mut relay_bid_to_remove: Option<i64> = None;
        let mut exp_site_to_clear: Option<String> = None;
        let mut pool_loot: Vec<(u32, i64)> = Vec::new();
        let mut relay_hacked = false;
        let details_opt = _db_read_and_clear_seasonal_details(db, iid);
        if let Some(details) = details_opt {
            if details.relay_ball_id != 0 {
                relay_bid_to_remove = Some(details.relay_ball_id);
            }
            if !details.exploration_site_id.is_empty() {
                exp_site_to_clear = Some(details.exploration_site_id);
            }
            pool_loot = details.crate_loot_pool;
            relay_hacked = details.relay_hacked;
        }
        let sys_handle = _systems.get_or_spawn(solar_system_id);
        for ebid in seasonal_escorts {
            sys_handle.try_remove_ball(ebid);
        }
        if let Some(relay_bid) = relay_bid_to_remove {
            sys_handle.try_remove_ball(relay_bid);
        }
        if let Some(exp_sid) = exp_site_to_clear {
            crate::services::pve_exploration::with_pve_exploration_reg_db(db, now_sec, |reg| {
                if let Some(sites) = reg.sites_by_system.get_mut(&solar_system_id) {
                    for s in sites.iter_mut() {
                        if s.site_id == exp_sid {
                            s.cleared = true;
                        }
                    }
                }
            });
        }
        let anom_key = format!("dng-{did}-{iid}");
        let spawned_cid = db.with_mut_snapshot(|snap| {
            if let Some(list) = snap.cosmic_anomalies.get_mut(&solar_system_id) {
                for a in list.iter_mut() {
                    if a.anomaly_id == anom_key {
                        a.cleared = true;
                    }
                }
            }
            if let Some(relay_bid) = relay_bid_to_remove {
                snap.transient_items.remove(&relay_bid);
                snap.item_positions.remove(&relay_bid);
            }
            if let Some(cid) = killer_char_id {
                let mut ship_id = 0;
                if let Some(ch) = snap.characters.get_mut(&cid) {
                    ch.balance = ch
                        .balance
                        .checked_add(IskCents::from_isk(reward_isk))
                        .unwrap_or(ch.balance);
                    let new_bal = ch.balance;
                    acct_changes.push((cid, new_bal.to_wire_f64()));
                    ship_id = ch.active_ship_id;
                    snap.append_journal(
                        cid,
                        "BountyPrizes",
                        IskCents::from_isk(reward_isk),
                        new_bal,
                        format!("Seasonal Event Site Completion ({dname})"),
                    );
                }
                let mut st = crate::services::season_manager::load_season_state_from_blobs(snap, cid);
                st.season_points = st.season_points.saturating_add(reward_points);
                crate::services::season_manager::save_season_state_to_blobs(snap, cid, &st);
                let mut loot_specs = Vec::new();
                if loot_tid != 0 && _sde.types.contains_key(&loot_tid) {
                    loot_specs.push((snap.alloc_entity_id(), loot_tid, 1));
                }
                if relay_hacked {
                    for (ptid, pqty) in pool_loot {
                        if ptid != loot_tid && pqty > 0 && _sde.types.contains_key(&ptid) {
                            let qty = pqty.clamp(1, i64::from(i32::MAX)) as i32;
                            loot_specs.push((snap.alloc_entity_id(), ptid, qty));
                        }
                    }
                }
                let booster_tid = if _sde.types.contains_key(&9950) { 9950 } else { 15457 };
                if booster_tid != loot_tid && _sde.types.contains_key(&booster_tid) {
                    loot_specs.push((snap.alloc_entity_id(), booster_tid, 1));
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
            }
        });
        if let Some(container_id) = spawned_cid {
            let cont_type = if _sde.types.contains_key(&17996) { 17996 } else { 23 };
            let sys_handle = _systems.get_or_spawn(solar_system_id);
            let _ = sys_handle
                .spawn_ball(make_site_npc_ball(container_id, cont_type, DVec3::new(1.5e11, 2.0e10, -1.0e11)))
                .await;
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
            .find(|(_, d)| d.exploration_site_id == site_id)
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
            .and_then(|list| list.iter().find(|s| s.instance_id == inst_id).map(|s| s.boss_npc_ball_id))
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

fn _db_read_and_clear_seasonal_details(
    db: &WorldRepository,
    instance_id: i64,
) -> Option<SeasonalSiteDetails> {
    db.with_mut_snapshot(|snap| {
        let mut map = load_seasonal_site_details_map(snap);
        let removed = map.remove(&instance_id);
        if removed.is_some() {
            save_seasonal_site_details_map(snap, &map);
        }
        removed
    })
}

/// Spawns seasonal event sites in `sys_id` during an active seasonal event window and despawns expired ones.
pub async fn ensure_seasonal_event_sites_for_system(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> Vec<crate::services::pve_special_sites::SeasonalEventSiteRecord> {
    let now_ft = ctx.now_blueos_filetime_ticks();
    let now_sec = ctx.now_unix();
    let active_season: Option<crate::services::season_fsd::SeasonConfig> = ctx.db.with_snapshot(|snap| {
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
    });
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

    if let Some(season) = active_season.filter(|_| active_sites.is_empty()) {
        let spec = resolve_seasonal_event_spec(&ctx.sde, &season, sys_id);

        let (new_site, relay_bid) = with_pve_special_reg(ctx, |reg| {
            reg.next_seq += 1;
            let inst_id = 1_895_000_000 + reg.next_seq;
            reg.next_seq += 1;
            let boss_bid = 1_895_500_000 + reg.next_seq;
            reg.next_seq += 1;
            let escort_bid_1 = 1_895_500_000 + reg.next_seq;
            reg.next_seq += 1;
            let escort_bid_2 = 1_895_500_000 + reg.next_seq;
            reg.next_seq += 1;
            let relay_bid = 1_895_500_000 + reg.next_seq;
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
            reg.seasonal_event_sites
                .entry(sys_id)
                .or_default()
                .push(rec.clone());
            (rec, relay_bid)
        });

        let pos = DVec3::new(1.5e11, 2.0e10, -1.0e11);
        let relay_pos = pos + DVec3::new(0.0, 2_200.0, 0.0);
        let exp_site_id = format!("SHK-{}-{}", new_site.dungeon_id, new_site.instance_id);
        let details = SeasonalSiteDetails {
            instance_id: new_site.instance_id,
            season_theme: spec.theme.label().to_string(),
            dungeon_id: new_site.dungeon_id,
            dungeon_name: new_site.dungeon_name.clone(),
            boss_type_id: new_site.boss_type_id,
            escort_type_id: spec.escort_type_id,
            relay_ball_id: relay_bid,
            relay_type_id: spec.relay_type_id,
            relay_hacked: false,
            exploration_site_id: exp_site_id.clone(),
            crate_type_id: spec.crate_type_id,
            crate_loot_pool: spec.crate_loot_pool.clone(),
        };

        ctx.db.with_mut_snapshot(|snap| {
            let mut details_map = load_seasonal_site_details_map(snap);
            details_map.insert(new_site.instance_id, details);
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
                    anomaly_id: format!("dng-{}-{}", new_site.dungeon_id, new_site.instance_id),
                    solar_system_id: sys_id,
                    anomaly_type: "SeasonalEventSite".to_string(),
                    name: new_site.dungeon_name.clone(),
                    position: [pos.x, pos.y, pos.z],
                    signal_strength: 1.0,
                    ore_types: Vec::new(),
                    remaining_volume: 0.0,
                    created_unix: now_sec,
                    lifetime_seconds: 86_400,
                    cleared: false,
                });
        });

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
                    position: [pos.x, pos.y, pos.z],
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

        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let boss_ball = make_site_npc_ball(
            new_site.boss_npc_ball_id,
            new_site.boss_type_id,
            pos + DVec3::new(2_500.0, 0.0, 0.0),
        );
        let escort_ball_1 = make_site_npc_ball(
            new_site.escort_ball_ids[0],
            spec.escort_type_id,
            pos + DVec3::new(-2_500.0, 0.0, 0.0),
        );
        let escort_ball_2 = make_site_npc_ball(
            new_site.escort_ball_ids[1],
            spec.escort_type_id,
            pos + DVec3::new(-3_200.0, 1_200.0, 0.0),
        );
        let mut relay_ball = make_site_npc_ball(relay_bid, spec.relay_type_id, relay_pos);
        relay_ball.max_velocity = 0.0;
        relay_ball.speed_fraction = 0.0;
        relay_ball.radius = 500.0;
        let boss_cs = build_special_site_npc_combat_state(&ctx.sde, new_site.boss_type_id);
        let escort_cs_1 = build_special_site_npc_combat_state(&ctx.sde, spec.escort_type_id);
        let escort_cs_2 = build_special_site_npc_combat_state(&ctx.sde, spec.escort_type_id);
        let _ = sys_handle.spawn_ball_with_combat(boss_ball, boss_cs).await;
        let _ = sys_handle.spawn_ball_with_combat(escort_ball_1, escort_cs_1).await;
        let _ = sys_handle.spawn_ball_with_combat(escort_ball_2, escort_cs_2).await;
        let _ = sys_handle.spawn_ball(relay_ball).await;
        active_sites.push(new_site);
    }

    active_sites
}

#[cfg(test)]
#[path = "pve_special_sites_combat_tests.rs"]
mod pve_special_sites_combat_tests;
#[cfg(test)]
#[path = "pve_special_sites_combat_edges_tests.rs"]
mod pve_special_sites_combat_edges_tests;

