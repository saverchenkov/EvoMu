//! `pve_special_sites_seasonal` — Seasonal Event Site Activation, Tick, Expiry & Reconciliation (`#1542`).
//!
//! Shares snapshot state and combat-death hooks with [`crate::services::pve_special_sites_combat`].

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::ExtendedNpcEntity;
use crate::services::pve_incursion_sites::make_site_npc_ball;
use crate::services::pve_special_sites::SeasonalEventSiteRecord;
use crate::services::pve_special_sites_combat::{
    active_season_in_snapshot, default_seasonal_site_position, load_seasonal_cooldown_store,
    load_seasonal_site_details_map, mutate_seasonal_exploration_reg_in_snap,
    remove_seasonal_exploration_sites_in_snap, resolve_seasonal_event_spec,
    save_seasonal_cooldown_store, save_seasonal_site_details_map, seasonal_site_valid_in_snap,
    seasonal_system_lock, SeasonalSiteDetails,
};
use crate::services::season_fsd::SeasonConfig;
use eve_db::WorldRepository;
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::collections::{BTreeMap, BTreeSet, HashMap};

type SeasonalNpcBuild = (
    eve_destiny::Ball,
    eve_destiny::CombatState,
    eve_destiny::SlimItemMeta,
    ExtendedNpcEntity,
);

struct PreparedSeasonalSites {
    active_season: Option<SeasonConfig>,
    expired_balls: Vec<i64>,
    expired_relay_balls: Vec<i64>,
    active_sites: Vec<SeasonalEventSiteRecord>,
    details_by_iid: BTreeMap<i64, SeasonalSiteDetails>,
    ai_present: BTreeSet<i64>,
}

struct MissingSeasonalSpawns {
    missing_relays: Vec<(i64, i64, u32, DVec3)>,
    missing_ballpark_npcs: Vec<(
        i64,
        i64,
        eve_destiny::Ball,
        eve_destiny::CombatState,
        eve_destiny::SlimItemMeta,
        ExtendedNpcEntity,
    )>,
    missing_ai_only: Vec<(i64, i64, ExtendedNpcEntity)>,
}

enum SeasonalSpawnReservation {
    Created {
        instance_id: i64,
        relay_bid: i64,
        relay_type_id: u32,
        relay_pos: DVec3,
        npc_spawns: Vec<SeasonalNpcBuild>,
    },
    AlreadyPresent(Vec<SeasonalEventSiteRecord>),
    CooldownBlocked,
    Inactive,
}

enum SeasonalBallTarget<'a, 'b> {
    SeedCtx(&'a mut crate::system_actor::SystemContentSeedContext<'b>),
    LiveHandle(&'a crate::system_actor::SystemHandle),
}

impl SeasonalBallTarget<'_, '_> {
    fn ball_pos(&self, bid: i64) -> Option<DVec3> {
        match self {
            Self::SeedCtx(s) => s.ballpark.balls.get(&bid).map(|b| b.position),
            Self::LiveHandle(h) => h.view().balls.get(&bid).map(|b| b.position),
        }
    }

    fn has_ball(&self, bid: i64) -> bool {
        self.ball_pos(bid).is_some()
    }

    fn spawn_npc(
        &mut self,
        ball: eve_destiny::Ball,
        cs: eve_destiny::CombatState,
        meta: eve_destiny::SlimItemMeta,
    ) {
        match self {
            Self::SeedCtx(s) => s.spawn_ball_with_meta(ball, cs, meta),
            Self::LiveHandle(h) => h.try_spawn_ball_with_meta(ball, cs, meta),
        }
    }

    fn spawn_relay(
        &mut self,
        sde: &eve_sde::SdeRegistry,
        bid: i64,
        tid: u32,
        pos: DVec3,
    ) {
        let (ball, cs, meta) = build_seasonal_relay_ball_parts(sde, bid, tid, pos);
        match self {
            Self::SeedCtx(s) => {
                s.spawn_ball_with_meta(ball, cs, meta);
                s.ballpark.mark_ball_non_free(bid, false);
            }
            Self::LiveHandle(h) => {
                h.try_spawn_ball_with_meta(ball, cs, meta);
                h.try_mark_ball_non_free(bid, false);
            }
        }
    }

    fn remove_ball(&mut self, bid: i64) {
        match self {
            Self::SeedCtx(s) => {
                let _ = s.ballpark.remove_ball(bid);
            }
            Self::LiveHandle(h) => h.try_remove_ball(bid),
        }
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
    entity: &mut ExtendedNpcEntity,
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

/// Builds `(Ball, CombatState, SlimItemMeta)` for a pinned stationary seasonal `SHK-*` data relay.
fn build_seasonal_relay_ball_parts(
    sde: &eve_sde::SdeRegistry,
    relay_bid: i64,
    relay_type_id: u32,
    relay_pos: DVec3,
) -> (
    eve_destiny::Ball,
    eve_destiny::CombatState,
    eve_destiny::SlimItemMeta,
) {
    let mut relay_ball = make_site_npc_ball(relay_bid, relay_type_id, relay_pos);
    relay_ball.max_velocity = 0.0;
    relay_ball.speed_fraction = 0.0;
    relay_ball.radius = 500.0;
    relay_ball.mode = eve_destiny::MovementMode::Rigid;
    let relay_cs =
        crate::services::pve_exploration_seeding::build_type_combat_state_from_sde(sde, relay_type_id);
    let (group_id, category_id, name) = sde
        .types
        .get(&relay_type_id)
        .map(|t| {
            (
                t.group_id,
                t.category_id,
                (!t.name.is_empty()).then(|| t.name.clone()),
            )
        })
        .unwrap_or((306, 11, None));
    let relay_meta = eve_destiny::SlimItemMeta {
        group_id,
        category_id,
        owner_id: 1,
        corp_id: -1,
        alliance_id: -1,
        name,
        ..Default::default()
    };
    (relay_ball, relay_cs, relay_meta)
}

/// Spawns the pinned stationary seasonal `SHK-*` data relay ball in `sys_handle`.
async fn spawn_seasonal_relay_ball(
    sys_handle: &crate::system_actor::SystemHandle,
    sde: &eve_sde::SdeRegistry,
    relay_bid: i64,
    relay_type_id: u32,
    relay_pos: DVec3,
) {
    let (relay_ball, relay_cs, relay_meta) =
        build_seasonal_relay_ball_parts(sde, relay_bid, relay_type_id, relay_pos);
    let _ = sys_handle
        .spawn_ball_with_meta(relay_ball, relay_cs, relay_meta)
        .await;
    let _ = sys_handle
        .emit_destiny_event(
            relay_bid,
            eve_destiny::DestinyEvent::SetBallFree {
                ball_id: relay_bid,
                is_free: false,
            },
        )
        .await;
}

/// Builds the full Ballpark + AI components for a seasonal NPC directly from [`eve_sde::SdeRegistry`]
/// without acquiring any DB locks.
fn build_seasonal_npc_full_for_sde(
    sde: &eve_sde::SdeRegistry,
    sys_id: SolarSystemId,
    ball_id: i64,
    type_id: u32,
    faction_id: u32,
    pos: DVec3,
) -> SeasonalNpcBuild {
    let archetype =
        crate::services::combat_crimewatch::combat_npc_ai::derive_npc_archetype_from_sde(
            sde, type_id,
        );
    let rng_seed = u64::from(sys_id).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (ball_id as u64);
    let (ball, cs, mut entity) =
        crate::combat::npc_ai_weapons_loot::build_specialized_npc_components_for_sde(
            sde,
            sys_id,
            ball_id,
            type_id,
            archetype,
            faction_id,
            pos,
            rng_seed,
        );
    configure_seasonal_npc_entity(&mut entity, faction_id);
    let sde_t = sde.types.get(&type_id);
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
        corp_id: crate::services::combat_crimewatch::combat_npc_ai::resolve_npc_corp_id_for_sde_faction(
            sde, fac_owner,
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

fn expire_and_prepare_seasonal_sites_in_locks(
    db: &WorldRepository,
    combat_ephemeral: &dyn eve_db::lock_rank::AnyRwLock<
        crate::services::combat_crimewatch::CombatRuntimeState,
    >,
    sys_id: SolarSystemId,
    now_ft: i64,
    now_sec: u64,
) -> PreparedSeasonalSites {
    let (active_season, expired_balls, expired_relay_balls, active_sites, details_by_iid, ai_present) =
        crate::services::combat_crimewatch::with_mut_combat_runtime_tx_locks(
            db,
            combat_ephemeral,
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
                            exp_sites.push(format!("SHK-{}-{}", s.dungeon_id, s.instance_id));
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
                            if !d.exploration_site_id.is_empty()
                                && !exp_sites.contains(&d.exploration_site_id)
                            {
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
                let mut ai_present = BTreeSet::new();
                for site in &active {
                    if let Some(d) = details_map.get(&site.instance_id) {
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
                        for &bid in
                            std::iter::once(&site.boss_npc_ball_id).chain(site.escort_ball_ids.iter())
                        {
                            if rt.npc_entities.contains_key(&bid) {
                                ai_present.insert(bid);
                            }
                        }
                    }
                }

                if let Err(err) = remove_seasonal_exploration_sites_in_snap(
                    snap,
                    sys_id,
                    now_sec,
                    &exp_sites,
                    &expired_relay_balls,
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
    PreparedSeasonalSites {
        active_season,
        expired_balls,
        expired_relay_balls,
        active_sites,
        details_by_iid,
        ai_present,
    }
}

fn collect_missing_seasonal_spawns_with<F>(
    sde: &eve_sde::SdeRegistry,
    sys_id: SolarSystemId,
    prepared: &PreparedSeasonalSites,
    mut get_existing_ball_pos: F,
) -> MissingSeasonalSpawns
where
    F: FnMut(i64) -> Option<DVec3>,
{
    let fallback_faction_id = prepared
        .active_season
        .as_ref()
        .map(|season| resolve_seasonal_event_spec(sde, season, sys_id).faction_id)
        .unwrap_or(500_012);
    let mut missing_relays = Vec::new();
    let mut missing_ballpark_npcs = Vec::new();
    let mut missing_ai_only = Vec::new();

    for site in &prepared.active_sites {
        let details = prepared.details_by_iid.get(&site.instance_id);
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
                && get_existing_ball_pos(d.relay_ball_id).is_none()
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
            let in_ai = prepared.ai_present.contains(&bid);
            match get_existing_ball_pos(bid) {
                Some(existing_pos) if !in_ai => {
                    let (_, _, _, entity) = build_seasonal_npc_full_for_sde(
                        sde, sys_id, bid, tid, site_faction_id, existing_pos,
                    );
                    missing_ai_only.push((site.instance_id, bid, entity));
                }
                Some(_) => {}
                None => {
                    let (ball, cs, npc_meta, entity) = build_seasonal_npc_full_for_sde(
                        sde, sys_id, bid, tid, site_faction_id, default_pos,
                    );
                    missing_ballpark_npcs.push((site.instance_id, bid, ball, cs, npc_meta, entity));
                }
            }
        }
    }

    MissingSeasonalSpawns {
        missing_relays,
        missing_ballpark_npcs,
        missing_ai_only,
    }
}

fn filter_surviving_seasonal_spawns(
    db: &WorldRepository,
    sys_id: SolarSystemId,
    missing: MissingSeasonalSpawns,
) -> (
    Vec<(i64, i64, u32, DVec3)>,
    Vec<(i64, i64, Option<ExtendedNpcEntity>)>,
    Vec<(
        i64,
        i64,
        eve_destiny::Ball,
        eve_destiny::CombatState,
        eve_destiny::SlimItemMeta,
        ExtendedNpcEntity,
    )>,
) {
    let now_ft_re = db.clock().now_blueos_filetime_ticks();
    let now_sec_re = db.clock().now_unix_secs();
    db.with_snapshot(|snap| {
        let active_season_id = active_season_in_snapshot(snap, now_ft_re).map(|s| s.season_id);
        let details_map = load_seasonal_site_details_map(snap);
        let sites = snap.pve_special_sites.seasonal_event_sites.get(&sys_id);
        let is_site_live = |iid: i64| {
            sites
                .and_then(|list| list.iter().find(|s| s.instance_id == iid))
                .is_some_and(|s| {
                    seasonal_site_valid_in_snap(
                        snap, sys_id, s, active_season_id, now_ft_re, now_sec_re,
                    )
                })
        };
        let is_npc_alive = |iid: i64, bid: i64| {
            sites
                .and_then(|list| list.iter().find(|s| s.instance_id == iid))
                .is_some_and(|s| {
                    seasonal_site_valid_in_snap(
                        snap, sys_id, s, active_season_id, now_ft_re, now_sec_re,
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

        let surviving_ai_only = missing
            .missing_ai_only
            .into_iter()
            .filter(|(iid, bid, _)| is_npc_alive(*iid, *bid))
            .map(|(iid, bid, ent)| (iid, bid, Some(ent)))
            .collect();
        let surviving_relays = missing
            .missing_relays
            .into_iter()
            .filter(|&(iid, relay_bid, _, _)| {
                is_site_live(iid)
                    && details_map
                        .get(&iid)
                        .is_some_and(|d| d.relay_ball_id == relay_bid)
            })
            .collect();
        let surviving_npcs = missing
            .missing_ballpark_npcs
            .into_iter()
            .filter(|(iid, bid, _, _, _, _)| is_npc_alive(*iid, *bid))
            .collect();
        (surviving_relays, surviving_ai_only, surviving_npcs)
    })
}

fn reconcile_post_spawn_seasonal_balls_with_locks<F>(
    db: &WorldRepository,
    combat_ephemeral: &dyn eve_db::lock_rank::AnyRwLock<
        crate::services::combat_crimewatch::CombatRuntimeState,
    >,
    sys_id: SolarSystemId,
    ballpark_has_ball: F,
    npc_entries: Vec<(i64, i64, Option<ExtendedNpcEntity>)>,
    spawned_relays: &[(i64, i64)],
) -> (Vec<i64>, Vec<SeasonalEventSiteRecord>)
where
    F: Fn(i64) -> bool,
{
    let now_ft = db.clock().now_blueos_filetime_ticks();
    let now_sec = db.clock().now_unix_secs();
    crate::services::combat_crimewatch::with_mut_combat_runtime_tx_locks(
        db,
        combat_ephemeral,
        |snap, rt| {
            let active_season_id = active_season_in_snapshot(snap, now_ft).map(|s| s.season_id);
            let details_map = load_seasonal_site_details_map(snap);
            let sites = snap.pve_special_sites.seasonal_event_sites.get(&sys_id);
            let mut stale = Vec::new();
            for (iid, npc_bid, entity_opt) in npc_entries {
                let npc_alive = sites
                    .and_then(|list| list.iter().find(|s| s.instance_id == iid))
                    .is_some_and(|s| {
                        seasonal_site_valid_in_snap(
                            snap, sys_id, s, active_season_id, now_ft, now_sec,
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
                } else if ballpark_has_ball(npc_bid) {
                    if let Some(entity) = entity_opt {
                        rt.npc_entities.entry(npc_bid).or_insert(entity);
                    }
                }
            }
            for &(iid, relay_bid) in spawned_relays {
                let relay_valid = sites
                    .and_then(|list| list.iter().find(|s| s.instance_id == iid))
                    .is_some_and(|s| {
                        seasonal_site_valid_in_snap(
                            snap, sys_id, s, active_season_id, now_ft, now_sec,
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
                                snap, sys_id, s, active_season_id, now_ft, now_sec,
                            )
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            (stale, valid_sites)
        },
    )
}

async fn reconcile_post_spawn_seasonal_balls(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    sys_id: SolarSystemId,
    npc_entries: Vec<(i64, i64, Option<ExtendedNpcEntity>)>,
    spawned_relays: &[(i64, i64)],
) -> Vec<SeasonalEventSiteRecord> {
    let view = sys_handle.view();
    let (stale_balls, valid_sites) = reconcile_post_spawn_seasonal_balls_with_locks(
        &ctx.db,
        &*ctx.combat_ephemeral,
        sys_id,
        |bid| view.balls.contains_key(&bid),
        npc_entries,
        spawned_relays,
    );
    for bid in stale_balls {
        sys_handle.try_remove_ball(bid);
    }
    valid_sites
}

fn reserve_seasonal_site_in_snap(
    sde: &eve_sde::SdeRegistry,
    db: &WorldRepository,
    sys_id: SolarSystemId,
    season: &SeasonConfig,
    now_sec: u64,
) -> SeasonalSpawnReservation {
    let cooldown_active = db.with_snapshot(|snap| {
        load_seasonal_cooldown_store(snap)
            .until_by_system
            .get(&sys_id)
            .copied()
            .is_some_and(|until| now_sec < until)
    });
    if cooldown_active {
        return SeasonalSpawnReservation::CooldownBlocked;
    }

    let pre_season_id = season.season_id;
    let pre_spec = resolve_seasonal_event_spec(sde, season, sys_id);

    db.with_mut_snapshot(|snap| {
        let now_ft_res = db.clock().now_blueos_filetime_ticks();
        let now_sec_res = db.clock().now_unix_secs();
        let Some(live_season) = active_season_in_snapshot(snap, now_ft_res) else {
            return SeasonalSpawnReservation::Inactive;
        };
        if snap.pve_special_sites.sim_clock_sec < now_sec_res {
            snap.pve_special_sites.sim_clock_sec = now_sec_res;
        }
        if let Some(existing) = snap.pve_special_sites.seasonal_event_sites.get(&sys_id) {
            let active_existing: Vec<_> = existing
                .iter()
                .filter(|s| {
                    seasonal_site_valid_in_snap(
                        snap, sys_id, s, Some(live_season.season_id), now_ft_res, now_sec_res,
                    )
                })
                .cloned()
                .collect();
            if !active_existing.is_empty() {
                return SeasonalSpawnReservation::AlreadyPresent(active_existing);
            }
        }
        let mut cooldowns = load_seasonal_cooldown_store(snap);
        if cooldowns
            .until_by_system
            .get(&sys_id)
            .copied()
            .is_some_and(|until| now_sec_res < until)
        {
            return SeasonalSpawnReservation::CooldownBlocked;
        }
        let spec = if live_season.season_id == pre_season_id {
            pre_spec
        } else {
            resolve_seasonal_event_spec(sde, &live_season, sys_id)
        };
        let seq_entry = cooldowns.spawn_seq_by_system.entry(sys_id).or_default();
        *seq_entry += 1;
        let spawn_slot = *seq_entry;
        save_seasonal_cooldown_store(snap, &cooldowns);

        let au_scale = 0.35 + 0.15 * ((spawn_slot % 3) as f64);
        let pos_arr =
            crate::services::sov_fw_insurgency::derive_system_site_position(sde, sys_id, spawn_slot, au_scale);
        let pos = DVec3::from_array(pos_arr);
        let relay_pos = pos + DVec3::new(0.0, 2_200.0, 0.0);

        snap.pve_special_sites.next_seq += 1;
        let inst_id = 1_895_000_000 + snap.pve_special_sites.next_seq;
        let boss_bid = snap.alloc_entity_id();
        let escort_bid_1 = snap.alloc_entity_id();
        let escort_bid_2 = snap.alloc_entity_id();
        let relay_bid = snap.alloc_entity_id();

        let rec = SeasonalEventSiteRecord {
            instance_id: inst_id,
            season_id: live_season.season_id,
            dungeon_id: spec.dungeon_id,
            dungeon_name: spec.dungeon_name.clone(),
            solar_system_id: sys_id,
            window_start_ft: live_season.start_time_ft,
            window_end_ft: live_season.end_time_ft,
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
                created_unix: now_sec_res,
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
            now_sec_res,
        );
        if let Err(err) = mutate_seasonal_exploration_reg_in_snap(snap, now_sec_res, |reg| {
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

        let npc_spawns = [
            (rec.boss_npc_ball_id, rec.boss_type_id, pos + DVec3::new(2_500.0, 0.0, 0.0)),
            (rec.escort_ball_ids[0], spec.escort_type_id, pos + DVec3::new(-2_500.0, 0.0, 0.0)),
            (rec.escort_ball_ids[1], spec.escort_type_id, pos + DVec3::new(-3_200.0, 1_200.0, 0.0)),
        ]
        .into_iter()
        .map(|(bid, tid, npc_pos)| {
            build_seasonal_npc_full_for_sde(sde, sys_id, bid, tid, spec.faction_id, npc_pos)
        })
        .collect();

        SeasonalSpawnReservation::Created {
            instance_id: rec.instance_id,
            relay_bid,
            relay_type_id: spec.relay_type_id,
            relay_pos,
            npc_spawns,
        }
    })
}

fn seasonal_system_needs_sync_ro<F>(
    db: &WorldRepository,
    combat_ephemeral: &dyn eve_db::lock_rank::AnyRwLock<
        crate::services::combat_crimewatch::CombatRuntimeState,
    >,
    sys_id: SolarSystemId,
    now_ft: i64,
    now_sec: u64,
    has_ball: F,
) -> bool
where
    F: Fn(i64) -> bool,
{
    let check = db.with_snapshot(|snap| {
        let sites = snap.pve_special_sites.seasonal_event_sites.get(&sys_id)?;
        if sites.is_empty() {
            return None;
        }
        let active_season_id = active_season_in_snapshot(snap, now_ft).map(|a| a.season_id);
        let details_map = load_seasonal_site_details_map(snap);
        let exp_reg_sites = snap
            .owner_notes
            .get(&crate::services::pve_exploration::PVE_EXPLORATION_NOTE_ID)
            .and_then(|note| {
                serde_json::from_str::<crate::services::pve_exploration::PveExplorationRegistry>(
                    &note.note,
                )
                .ok()
            })
            .and_then(|mut exp_reg| exp_reg.sites_by_system.remove(&sys_id));

        let mut surviving_npcs = Vec::new();
        let mut defeated_npcs = Vec::new();

        for s in sites {
            if !seasonal_site_valid_in_snap(snap, sys_id, s, active_season_id, now_ft, now_sec) {
                return Some((true, Vec::new(), Vec::new()));
            }
            let Some(d) = details_map.get(&s.instance_id) else {
                return Some((true, Vec::new(), Vec::new()));
            };
            if d.relay_ball_id != 0 && d.relay_type_id != 0 {
                if (!snap.transient_items.contains_key(&d.relay_ball_id)
                    && !snap.items.contains_key(&d.relay_ball_id))
                    || !snap.item_positions.contains_key(&d.relay_ball_id)
                    || !has_ball(d.relay_ball_id)
                {
                    return Some((true, Vec::new(), Vec::new()));
                }
                if !d.exploration_site_id.is_empty()
                    && exp_reg_sites.as_ref().is_none_or(|l| {
                        !l.iter().any(|es| es.site_id == d.exploration_site_id)
                    })
                {
                    return Some((true, Vec::new(), Vec::new()));
                }
            }
            if d.boss_defeated {
                defeated_npcs.push(s.boss_npc_ball_id);
            } else {
                if !has_ball(s.boss_npc_ball_id) {
                    return Some((true, Vec::new(), Vec::new()));
                }
                surviving_npcs.push(s.boss_npc_ball_id);
            }
            for &ebid in &s.escort_ball_ids {
                if d.defeated_escort_ball_ids.contains(&ebid) {
                    defeated_npcs.push(ebid);
                } else {
                    if !has_ball(ebid) {
                        return Some((true, Vec::new(), Vec::new()));
                    }
                    surviving_npcs.push(ebid);
                }
            }
        }
        Some((false, surviving_npcs, defeated_npcs))
    });

    let Some((needs_sync, surviving_npcs, defeated_npcs)) = check else {
        return false;
    };
    if needs_sync {
        return true;
    }
    let rt = combat_ephemeral.read().unwrap_or_else(|e| e.into_inner());
    surviving_npcs.iter().any(|bid| !rt.npc_entities.contains_key(bid))
        || defeated_npcs.iter().any(|bid| rt.npc_entities.contains_key(bid))
}

fn sync_seasonal_event_sites_with_target(
    sde: &eve_sde::SdeRegistry,
    db: &WorldRepository,
    combat_ephemeral: &dyn eve_db::lock_rank::AnyRwLock<
        crate::services::combat_crimewatch::CombatRuntimeState,
    >,
    mut target: SeasonalBallTarget<'_, '_>,
    sys_id: SolarSystemId,
    now_sec: u64,
) {
    let now_ft = db.clock().now_blueos_filetime_ticks();
    if !seasonal_system_needs_sync_ro(db, combat_ephemeral, sys_id, now_ft, now_sec, |bid| {
        target.has_ball(bid)
    }) {
        return;
    }
    let prepared = expire_and_prepare_seasonal_sites_in_locks(
        db, combat_ephemeral, sys_id, now_ft, now_sec,
    );
    for bid in prepared
        .expired_balls
        .iter()
        .chain(prepared.expired_relay_balls.iter())
    {
        target.remove_ball(*bid);
    }

    if !prepared.active_sites.is_empty() {
        let missing = collect_missing_seasonal_spawns_with(sde, sys_id, &prepared, |bid| {
            target.ball_pos(bid)
        });
        let (surviving_relays, surviving_ai_only, surviving_npcs) =
            filter_surviving_seasonal_spawns(db, sys_id, missing);
        let mut spawned_relays = Vec::with_capacity(surviving_relays.len());
        for (iid, relay_bid, relay_tid, relay_pos) in surviving_relays {
            target.spawn_relay(sde, relay_bid, relay_tid, relay_pos);
            spawned_relays.push((iid, relay_bid));
        }
        let mut npc_checks = surviving_ai_only;
        for (iid, bid, ball, cs, npc_meta, entity) in surviving_npcs {
            target.spawn_npc(ball, cs, npc_meta);
            npc_checks.push((iid, bid, Some(entity)));
        }
        if !npc_checks.is_empty() || !spawned_relays.is_empty() {
            let (stale_balls, _) = reconcile_post_spawn_seasonal_balls_with_locks(
                db,
                combat_ephemeral,
                sys_id,
                |bid| target.has_ball(bid),
                npc_checks,
                &spawned_relays,
            );
            for bid in stale_balls {
                target.remove_ball(bid);
            }
        }
    }
}

/// Synchronizes seasonal event sites during `Cold -> Loading -> Live` activation (`#1542`).
pub(crate) fn sync_seasonal_event_sites_in_seed_ctx(
    seed_ctx: &mut crate::system_actor::SystemContentSeedContext<'_>,
    sde: &eve_sde::SdeRegistry,
    db: &WorldRepository,
    combat_ephemeral: &dyn eve_db::lock_rank::AnyRwLock<
        crate::services::combat_crimewatch::CombatRuntimeState,
    >,
) {
    if !seed_ctx.db_read_safe {
        return;
    }
    let sys_id = seed_ctx.solar_system_id;
    let now_sec = seed_ctx.now_unix_secs();
    sync_seasonal_event_sites_with_target(
        sde,
        db,
        combat_ephemeral,
        SeasonalBallTarget::SeedCtx(seed_ctx),
        sys_id,
        now_sec,
    );
}

/// Expires stale or out-of-window seasonal event sites for `sys_id` synchronously (`#1542`).
pub(crate) fn expire_seasonal_event_sites_for_system_sync(
    db: &WorldRepository,
    combat_ephemeral: &dyn eve_db::lock_rank::AnyRwLock<
        crate::services::combat_crimewatch::CombatRuntimeState,
    >,
    handle_opt: Option<&crate::system_actor::SystemHandle>,
    sys_id: SolarSystemId,
    now_sec: u64,
) {
    let now_ft = db.clock().now_blueos_filetime_ticks();
    let has_expired = db.with_snapshot(|snap| {
        let active_season_id = active_season_in_snapshot(snap, now_ft).map(|a| a.season_id);
        snap.pve_special_sites
            .seasonal_event_sites
            .get(&sys_id)
            .is_some_and(|list| {
                list.iter().any(|s| {
                    !seasonal_site_valid_in_snap(
                        snap,
                        sys_id,
                        s,
                        active_season_id,
                        now_ft,
                        now_sec,
                    )
                })
            })
    });
    if !has_expired {
        return;
    }
    let prepared = expire_and_prepare_seasonal_sites_in_locks(
        db, combat_ephemeral, sys_id, now_ft, now_sec,
    );
    if let Some(handle) = handle_opt {
        for bid in prepared
            .expired_balls
            .into_iter()
            .chain(prepared.expired_relay_balls.into_iter())
        {
            handle.try_remove_ball(bid);
        }
    }
}

/// Maintains seasonal event sites on an already-live system (`tick` provider hook, `#1542`).
pub(crate) fn tick_seasonal_event_sites_for_system_sync(
    sde: &eve_sde::SdeRegistry,
    db: &WorldRepository,
    combat_ephemeral: &dyn eve_db::lock_rank::AnyRwLock<
        crate::services::combat_crimewatch::CombatRuntimeState,
    >,
    handle: &crate::system_actor::SystemHandle,
    sys_id: SolarSystemId,
    now_sec: u64,
) {
    let has_sites = db.with_snapshot(|snap| {
        snap.pve_special_sites
            .seasonal_event_sites
            .get(&sys_id)
            .is_some_and(|list| !list.is_empty())
    });
    if !has_sites {
        return;
    }
    sync_seasonal_event_sites_with_target(
        sde,
        db,
        combat_ephemeral,
        SeasonalBallTarget::LiveHandle(handle),
        sys_id,
        now_sec,
    );
}

/// Ensures seasonal event sites for `sys_id` ONLY when `sys_id` is already loaded/live; fails
/// closed with no actor wake and no DB write if `sys_id` is cold (`#1542`).
pub(crate) async fn ensure_seasonal_event_sites_if_live_for_system(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> Vec<SeasonalEventSiteRecord> {
    ensure_seasonal_event_sites_inner(ctx, sys_id, false).await
}

/// Spawns seasonal event sites in `sys_id` during an active seasonal event window (subject to
/// persisted per-system cooldown) and despawns completed, expired, or out-of-window ones.
#[cfg(any(test, feature = "testkit"))]
pub async fn ensure_seasonal_event_sites_for_system(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> Vec<SeasonalEventSiteRecord> {
    ensure_seasonal_event_sites_inner(ctx, sys_id, true).await
}

async fn ensure_seasonal_event_sites_inner(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    allow_wake: bool,
) -> Vec<SeasonalEventSiteRecord> {
    if !allow_wake && ctx.systems.get_handle(sys_id).is_none() {
        return Vec::new();
    }
    let sys_lock = seasonal_system_lock(&ctx.db, sys_id);
    let _sys_guard = sys_lock.lock().await;

    let now_ft = ctx.now_blueos_filetime_ticks();
    let now_sec = ctx.now_unix();

    let (has_sites, has_active_season) = ctx.db.with_snapshot(|snap| {
        (
            snap.pve_special_sites
                .seasonal_event_sites
                .get(&sys_id)
                .is_some_and(|list| !list.is_empty()),
            active_season_in_snapshot(snap, now_ft).is_some(),
        )
    });
    if !has_sites && !has_active_season {
        return Vec::new();
    }

    let sys_handle = if allow_wake {
        let Ok(h) = ctx
            .systems
            .wake_for_gm_command_async(sys_id, "EnsureSeasonalEventSites")
            .await
        else {
            return Vec::new();
        };
        h
    } else {
        let Some(h) = ctx.systems.get_handle(sys_id) else {
            return Vec::new();
        };
        h
    };

    let prepared = expire_and_prepare_seasonal_sites_in_locks(
        &ctx.db, &*ctx.combat_ephemeral, sys_id, now_ft, now_sec,
    );

    if !prepared.expired_balls.is_empty() || !prepared.expired_relay_balls.is_empty() {
        for bid in prepared
            .expired_balls
            .iter()
            .chain(prepared.expired_relay_balls.iter())
        {
            sys_handle.try_remove_ball(*bid);
        }
    }

    // Re-materialize only living, non-defeated NPCs and missing relay balls for uncompleted sites
    // missing from `rt.npc_entities` or Ballpark, re-validating against live DB state before
    // writing AI/Ballpark state so killed NPCs never resurrect.
    if !prepared.active_sites.is_empty() {
        let mut candidate_positions: HashMap<i64, DVec3> = HashMap::new();
        for site in &prepared.active_sites {
            if let Some(d) = prepared.details_by_iid.get(&site.instance_id) {
                if d.relay_ball_id != 0 {
                    if let Ok(Some((_, b))) = sys_handle.get_ball_snapshot(d.relay_ball_id).await {
                        candidate_positions.insert(d.relay_ball_id, b.position);
                    }
                }
            }
            for &bid in std::iter::once(&site.boss_npc_ball_id).chain(site.escort_ball_ids.iter())
            {
                if let Ok(Some((_, b))) = sys_handle.get_ball_snapshot(bid).await {
                    candidate_positions.insert(bid, b.position);
                }
            }
        }
        let missing = collect_missing_seasonal_spawns_with(&ctx.sde, sys_id, &prepared, |bid| {
            candidate_positions.get(&bid).copied()
        });
        let (surviving_relays, surviving_ai_only, surviving_ballpark_npcs) =
            filter_surviving_seasonal_spawns(&ctx.db, sys_id, missing);

        let mut spawned_relay_checks = Vec::with_capacity(surviving_relays.len());
        for (iid, relay_bid, relay_tid, relay_pos) in surviving_relays {
            spawn_seasonal_relay_ball(&sys_handle, &ctx.sde, relay_bid, relay_tid, relay_pos).await;
            spawned_relay_checks.push((iid, relay_bid));
        }
        let mut spawned_npc_checks = surviving_ai_only;
        for (iid, bid, ball, cs, npc_meta, entity) in surviving_ballpark_npcs {
            let _ = sys_handle.spawn_ball_with_meta(ball, cs, npc_meta).await;
            spawned_npc_checks.push((iid, bid, Some(entity)));
        }
        if spawned_npc_checks.is_empty() && spawned_relay_checks.is_empty() {
            return prepared.active_sites;
        }
        return reconcile_post_spawn_seasonal_balls(
            ctx, &sys_handle, sys_id, spawned_npc_checks, &spawned_relay_checks,
        )
        .await;
    }

    if let Some(season) = prepared.active_season {
        if !allow_wake && ctx.systems.get_handle(sys_id).is_none() {
            return prepared.active_sites;
        }
        match reserve_seasonal_site_in_snap(&ctx.sde, &ctx.db, sys_id, &season, now_sec) {
            SeasonalSpawnReservation::AlreadyPresent(existing) => return existing,
            SeasonalSpawnReservation::CooldownBlocked | SeasonalSpawnReservation::Inactive => {
                return prepared.active_sites;
            }
            SeasonalSpawnReservation::Created {
                instance_id,
                relay_bid,
                relay_type_id,
                relay_pos,
                npc_spawns,
            } => {
                let mut check_npcs = Vec::with_capacity(npc_spawns.len());
                let check_relays = [(instance_id, relay_bid)];
                for (ball, cs, npc_meta, entity) in npc_spawns {
                    let bid = ball.id;
                    let _ = sys_handle.spawn_ball_with_meta(ball, cs, npc_meta).await;
                    check_npcs.push((instance_id, bid, Some(entity)));
                }
                spawn_seasonal_relay_ball(&sys_handle, &ctx.sde, relay_bid, relay_type_id, relay_pos)
                    .await;
                return reconcile_post_spawn_seasonal_balls(
                    ctx, &sys_handle, sys_id, check_npcs, &check_relays,
                )
                .await;
            }
        }
    }

    prepared.active_sites
}
