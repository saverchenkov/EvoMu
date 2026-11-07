//! V24 SDE-backed helpers for Drifter Hives, Homefront Operations, Metaliminal Roaming Storms,
//! and Triglavian / EDENCOM sites (`feat:pve.drifter-hives`, `feat:pve.homefront-ops`,
//! `feat:pve.metaliminal-storm-movement`, `feat:pve.triglavian-sites`,
//! `rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances`,
//! `rpc:dungeonInstanceCacheMgr.GetTriglavianSiteInstances`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::pve_metaliminal_storms::{
    compute_storm_footprint, storm_move_interval_secs, storm_weather_beacons, RoamingStormRecord,
};
use crate::services::pve_special_sites::{
    next_spec_seq, spec_now_sec, AbyssalWeatherKind, DrifterHiveRecord, DrifterHiveRoom,
    PveSpecialSitesRegistry, TriglavianSiteRecord,
};
use eve_db::IskCents;
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId, TypeId};
use std::collections::{HashMap, HashSet};

/// Resolved V24 SDE Homefront operation template.
#[derive(Debug, Clone)]
pub struct SdeHomefrontTemplate {
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub difficulty: u32,
}

/// Resolves all V24 SDE Homefront Operations (`archetype_id == 70`) sorted by dungeon ID.
pub fn resolve_sde_homefront_templates(sde: &SdeRegistry) -> Vec<SdeHomefrontTemplate> {
    let ops_map = sde.operations().ok();
    let mut templates: Vec<SdeHomefrontTemplate> = sde
        .dungeons
        .values()
        .filter(|d| d.archetype_id == Some(70))
        .map(|d| {
            let localized_name = ops_map
                .as_ref()
                .and_then(|m| m.get(&d.dungeon_id.to_string()))
                .and_then(|v| v.get("operationNameID").and_then(|id| id.as_u64()))
                .and_then(|msg_id| sde.localized_message_en_us(msg_id as u32))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| d.name.clone());
            SdeHomefrontTemplate {
                dungeon_id: d.dungeon_id,
                dungeon_name: localized_name,
                difficulty: d.difficulty.unwrap_or(2),
            }
        })
        .collect();
    templates.sort_by_key(|t| t.dungeon_id);
    templates
}

/// Selects faction-matched Homefront hostile NPC boss `TypeId`s from V24 `sde.types`
/// based on the system's sovereign/resident faction and operation theme.
pub fn resolve_homefront_faction_boss_types(
    sde: &SdeRegistry,
    sys_id: SolarSystemId,
) -> (u32, u32) {
    let sys_faction = sde
        .solar_systems
        .get(&sys_id)
        .and_then(|s| {
            sde.constellations
                .get(&s.constellation_id)
                .and_then(|c| c.faction_id)
                .or_else(|| sde.regions.get(&s.region_id).and_then(|r| r.faction_id))
                .or_else(|| {
                    s.stations
                        .iter()
                        .find_map(|st| st.owner_faction_id.map(|f| f as u32))
                })
        })
        .unwrap_or(500_001);

    let hostile_faction: u32 = match sys_faction {
        500_003 => 500_012, // Amarr -> Blood Raiders
        500_004 => 500_020, // Gallente -> Serpentis
        500_002 => 500_011, // Minmatar -> Angel Cartel
        500_001 => 500_010, // Caldari -> Guristas
        _ => 500_019,       // Sansha's Nation
    };

    let mut cruiser_cands: Vec<u32> = sde
        .types
        .values()
        .filter(|t| {
            t.published
                && t.faction_id == Some(hostile_faction)
                && matches!(t.group_id, 550..=590 | 789..=815)
                && t.default_attributes
                    .get(&eve_sde::attr_ids::SHIELD_CAPACITY)
                    .copied()
                    .unwrap_or(0.0)
                    > 0.0
        })
        .map(|t| t.type_id)
        .collect();
    cruiser_cands.sort_unstable();
    let boss1 = cruiser_cands.first().copied().unwrap_or(48_090);
    let boss2 = cruiser_cands.get(1).copied().unwrap_or(boss1);
    (boss1, boss2)
}

/// Computes Homefront base pilot ISK payout grounded in V24 SDE dungeon difficulty & system security.
pub fn compute_homefront_base_pilot_isk(
    sde: &SdeRegistry,
    sys_id: SolarSystemId,
    difficulty: u32,
    slot_index: usize,
) -> i64 {
    let sec = sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.5)
        .clamp(0.45, 1.0);
    let sec_mult = 1.0 + ((1.0 - sec) * 0.25);
    let base = match difficulty {
        0 | 1 => 15_500_000.0,
        2 => 17_230_000.0,
        3 => 18_500_000.0,
        _ => 20_000_000.0,
    };
    let slot_adj = if slot_index % 2 == 0 { 1.0 } else { 0.90 };
    ((base * sec_mult * slot_adj) / 10_000.0).round() as i64 * 10_000
}

/// Computes live space-combat Homefront payout per pilot: full payout (`1.0x`) for up to
/// `optimal_fleet_size` pilots (`<= 5`), and proportional split `(optimal / fleet_count)` above `5`.
pub fn compute_homefront_combat_pilot_isk(
    base_pilot_isk: i64,
    optimal_fleet_size: usize,
    fleet_count: usize,
) -> i64 {
    let eff = fleet_count.max(1);
    let opt = optimal_fleet_size.max(1);
    let mult = if eff <= opt {
        1.0
    } else {
        (opt as f64) / (eff as f64)
    };
    ((base_pilot_isk as f64) * mult).round() as i64
}

/// Seeds all 5 Drifter Hives from V24 SDE `sde.dungeons` (`5642, 5648..=5651`), localized names,
/// Drifter NPC types (`sde.types`), and Antikythera Elements (`34556..=34560`).
pub fn init_drifter_hives_from_sde(sde: &SdeRegistry, reg: &mut PveSpecialSitesRegistry) {
    let defs: &[(SolarSystemId, u32, &str, TypeId, &str)] = &[
        (31_000_001, 5648, "Barbican Hive", 34_557, "Barbican Element"),
        (31_000_002, 5649, "Vidette Hive", 34_558, "Vidette Element"),
        (31_000_003, 5650, "Conflux Hive", 34_559, "Conflux Element"),
        (31_000_004, 5651, "Redoubt Hive", 34_560, "Redoubt Element"),
        (31_000_005, 5642, "Sentinel Hive", 34_556, "Sentinel Element"),
    ];

    let mut drifter_cruisers: Vec<u32> = sde
        .types
        .values()
        .filter(|t| t.published && t.group_id == 1666)
        .map(|t| t.type_id)
        .collect();
    drifter_cruisers.sort_unstable();
    let r1_a = drifter_cruisers.first().copied().unwrap_or(47_958);
    let r1_b = drifter_cruisers.get(1).copied().unwrap_or(47_959);

    let mut drifter_bs: Vec<u32> = sde
        .types
        .values()
        .filter(|t| t.published && t.group_id == 1667)
        .map(|t| t.type_id)
        .collect();
    drifter_bs.sort_unstable();
    let r2_a = drifter_bs.first().copied().unwrap_or(47_956);
    let r2_b = drifter_bs.get(1).copied().unwrap_or(47_957);

    let warden_tid = sde
        .types
        .get(&37_473)
        .map(|t| t.type_id)
        .unwrap_or(37_473);

    for &(sys_id, did, fallback_dname, elem_tid, fallback_elem_name) in defs {
        if reg.drifter_hives.contains_key(&sys_id) {
            continue;
        }
        let dname = sde
            .dungeons
            .get(&did)
            .map(|d| d.name.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| fallback_dname.to_string());
        let elem_name = sde
            .types
            .get(&elem_tid)
            .map(|t| t.name.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| fallback_elem_name.to_string());

        let base_ball = 1_898_000_000 + ((did as i64) * 100);
        let warden_npc_ball_id = base_ball + 30;
        let rooms = vec![
            DrifterHiveRoom {
                room_index: 1,
                room_name: format!("{dname} — Exterior Perimeter"),
                npc_ball_ids: vec![base_ball + 11, base_ball + 12],
                npc_type_ids: vec![r1_a, r1_b],
                beacon_ball_id: base_ball + 10,
                cleared: false,
                destroyed_npc_ball_ids: Vec::new(),
            },
            DrifterHiveRoom {
                room_index: 2,
                room_name: format!("{dname} — Tactical Muster"),
                npc_ball_ids: vec![base_ball + 21, base_ball + 22],
                npc_type_ids: vec![r2_a, r2_b],
                beacon_ball_id: base_ball + 20,
                cleared: false,
                destroyed_npc_ball_ids: Vec::new(),
            },
            DrifterHiveRoom {
                room_index: 3,
                room_name: format!("{dname} — Inner Sanctuary"),
                npc_ball_ids: vec![warden_npc_ball_id],
                npc_type_ids: vec![warden_tid],
                beacon_ball_id: base_ball + 31,
                cleared: false,
                destroyed_npc_ball_ids: Vec::new(),
            },
        ];
        reg.drifter_hives.insert(
            sys_id,
            DrifterHiveRecord {
                site_id: format!("DRIFTER-HIVE-{did}"),
                dungeon_id: did,
                dungeon_name: dname,
                solar_system_id: sys_id,
                element_type_id: elem_tid,
                element_name: elem_name,
                current_room: 1,
                total_rooms: 3,
                warden_npc_ball_id,
                cleared: false,
                rooms,
            },
        );
    }
}

/// Ensures the full set of 8 roaming null-sec Metaliminal Storms (2 each of Gamma, Electrical,
/// Plasma/Firestorm, Exotic) is populated using V24 SDE null-sec systems and stargate topology.
pub fn ensure_full_roaming_storm_set(ctx: &ServerContext, reg: &mut PveSpecialSitesRegistry) {
    crate::services::pve_metaliminal_storms::init_roaming_storms(ctx, reg);
    if reg.roaming_storms.len() >= 8 {
        return;
    }
    let now_sec = spec_now_sec(reg);
    let weathers = [
        AbyssalWeatherKind::Gamma,
        AbyssalWeatherKind::Electrical,
        AbyssalWeatherKind::Firestorm,
        AbyssalWeatherKind::Exotic,
    ];
    let mut used_regions: HashSet<u32> = reg
        .roaming_storms
        .iter()
        .filter_map(|s| {
            ctx.sde
                .solar_systems
                .get(&s.center_solar_system_id)
                .map(|sys| sys.region_id)
        })
        .collect();
    let mut null_candidates: Vec<(u32, SolarSystemId)> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| {
            crate::services::pve_metaliminal_storms::is_nullsec_storm_system(
                &ctx.sde,
                s.solar_system_id,
            )
        })
        .map(|s| (s.region_id, s.solar_system_id))
        .collect();
    null_candidates.sort_unstable();

    let mut next_id: u32 = reg
        .roaming_storms
        .iter()
        .map(|s| s.storm_id)
        .max()
        .unwrap_or(0)
        + 1;

    for &weather in &weathers {
        let existing_count = reg
            .roaming_storms
            .iter()
            .filter(|s| s.weather == weather)
            .count();
        for _ in existing_count..2 {
            let center_opt = null_candidates
                .iter()
                .find(|&&(rid, _)| !used_regions.contains(&rid))
                .or_else(|| {
                    null_candidates.iter().find(|&&(_, sid)| {
                        !reg.roaming_storms
                            .iter()
                            .any(|s| s.center_solar_system_id == sid)
                    })
                })
                .copied();
            let Some((rid, center_sys)) = center_opt else {
                break;
            };
            used_regions.insert(rid);
            let (core, periphery) = compute_storm_footprint(ctx, center_sys);
            let sid = next_id;
            next_id += 1;
            reg.roaming_storms.push(RoamingStormRecord {
                storm_id: sid,
                weather,
                center_solar_system_id: center_sys,
                strong_core_systems: core,
                weak_periphery_systems: periphery,
                step_count: 0,
                next_move_at_sec: now_sec + storm_move_interval_secs(sid, 0),
                recent_centers: vec![center_sys],
            });
        }
    }
}

/// Broadcasts live ship dogma `OnAttributes` notifications to pilots in storm-affected systems
/// when roaming storm beacons are added or removed.
pub fn notify_storm_ship_dogma_updates(
    ctx: &ServerContext,
    affected_systems: &[SolarSystemId],
    effect_lists_by_sys: &HashMap<SolarSystemId, Vec<TypeId>>,
) {
    if affected_systems.is_empty() {
        return;
    }
    let affected_set: HashSet<SolarSystemId> = affected_systems.iter().copied().collect();
    let targets: Vec<(i64, i64, usize, bool)> = ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .filter(|c| {
                c.station_id.is_none()
                    && c.structure_id.is_none()
                    && c.active_ship_id > 0
                    && affected_set.contains(&c.solar_system_id)
            })
            .map(|c| {
                let effects = effect_lists_by_sys
                    .get(&c.solar_system_id)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                let is_strong = effects.iter().any(|&tid| {
                    [
                        storm_weather_beacons(AbyssalWeatherKind::Gamma).1,
                        storm_weather_beacons(AbyssalWeatherKind::Electrical).1,
                        storm_weather_beacons(AbyssalWeatherKind::Firestorm).1,
                        storm_weather_beacons(AbyssalWeatherKind::Exotic).1,
                    ]
                    .contains(&tid)
                });
                (c.character_id, c.active_ship_id, effects.len(), is_strong)
            })
            .collect()
    });

    for (cid, ship_id, storm_count, is_strong) in targets {
        let strength_mult = if storm_count == 0 {
            1.0
        } else if is_strong {
            1.25 * (storm_count as f64)
        } else {
            1.10 * (storm_count as f64)
        };
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnAttributes",
            "charid",
            vec![PyRep::Dict(vec![(
                PyRep::Integer(ship_id),
                PyRep::Dict(vec![(
                    PyRep::String("metaliminalStormEffectScale".into()),
                    PyRep::Real(strength_mult),
                )]),
            )])],
        );
    }
}

/// Applies Triglavian / EDENCOM site rewards using EVE's asymptotic Social-skilled standing
/// formula (`new = cur + (10 - cur) * 0.015 * (1 + 0.05 * social_lvl)`), credits ISK + journal,
/// and emits `OnCharacterStandingChange` notification.
pub fn apply_triglavian_standings_and_isk(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    gain_faction: i64,
    lose_faction: i64,
    base_isk: i64,
    dungeon_name: &str,
) -> (Option<f64>, Option<(f64, f64)>) {
    let social_lvl = snap
        .skills
        .get(&char_id)
        .and_then(|skills| skills.get(&3355))
        .map(|s| s.level.clamp(0, 5))
        .unwrap_or(0);
    let social_mult = 1.0 + 0.05 * f64::from(social_lvl);

    let mut new_bal_isk = None;
    let mut standing_pair = None;
    if let Some(ch) = snap.characters.get_mut(&char_id) {
        let g = ch.standings.entry(gain_faction).or_insert(0.0);
        let next_g = (*g + (10.0 - *g) * 0.015 * social_mult).clamp(-10.0, 10.0);
        *g = next_g;

        let l = ch.standings.entry(lose_faction).or_insert(0.0);
        let next_l = (*l - (10.0 + *l) * 0.015).clamp(-10.0, 10.0);
        *l = next_l;
        standing_pair = Some((next_g, next_l));

        ch.balance = ch
            .balance
            .checked_add(IskCents::from_isk(base_isk))
            .unwrap_or(ch.balance);
        let new_bal = ch.balance;
        new_bal_isk = Some(new_bal.to_wire_f64());
        snap.append_journal(
            char_id,
            "CorporateRewardPayout",
            IskCents::from_isk(base_isk),
            new_bal,
            format!("Triglavian site payout ({dungeon_name})"),
        );
    }
    (new_bal_isk, standing_pair)
}

/// Resolves Triglavian / EDENCOM site records using V24 SDE dungeon names & NPC types.
pub fn build_triglavian_sites_from_sde(
    sde: &SdeRegistry,
    sys_id: SolarSystemId,
    is_edencom: bool,
    empire_navy: (u32, u32, u32),
) -> Vec<TriglavianSiteRecord> {
    let base_inst = (sys_id as i64) * 100;
    let is_pochven = sde
        .solar_systems
        .get(&sys_id)
        .is_some_and(|s| s.region_id == 10_000_070);
    let dng_name = |did: u32, default: &str| -> String {
        sde.dungeons
            .get(&did)
            .map(|d| d.name.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default.to_string())
    };
    let make_site = |inst, did: u32, fallback_name: &str, fac, isk, drop, boss_tid, esc_tids: Vec<u32>| {
        let esc_bids = (1..=esc_tids.len())
            .map(|i| 1_897_700_000 + inst * 10 + (i as i64))
            .collect();
        TriglavianSiteRecord {
            instance_id: inst,
            dungeon_id: did,
            dungeon_name: dng_name(did, fallback_name),
            solar_system_id: sys_id,
            faction_owner_id: fac,
            base_isk: isk,
            survey_db_drop: drop,
            completed: false,
            boss_npc_ball_id: 1_897_700_000 + inst,
            boss_type_id: boss_tid,
            escort_ball_ids: esc_bids,
            escort_type_ids: esc_tids,
            beacon_ball_id: 1_897_700_000 + inst * 10 + 9,
        }
    };

    if is_edencom {
        let (bs, bc, fr) = empire_navy;
        vec![
            make_site(
                base_inst + 1,
                8808,
                "EDENCOM Field Base",
                500_027,
                35_000_000,
                150,
                bs,
                vec![bc, fr],
            ),
            make_site(
                base_inst + 2,
                8792,
                "EDENCOM Forward Post",
                500_027,
                25_000_000,
                100,
                bc,
                vec![fr],
            ),
        ]
    } else if is_pochven {
        vec![
            make_site(
                base_inst + 1,
                8802,
                "Observatory Flashpoint",
                500_026,
                40_000_000,
                200,
                48_122,
                vec![48_086, 48_092],
            ),
            make_site(
                base_inst + 2,
                8742,
                "Incipient Drone Swarm",
                500_026,
                25_000_000,
                120,
                48_126,
                vec![48_088],
            ),
        ]
    } else {
        vec![
            make_site(
                base_inst + 1,
                8802,
                "Major Conduit",
                500_026,
                35_000_000,
                150,
                48_122,
                vec![48_086, 48_092],
            ),
            make_site(
                base_inst + 2,
                8742,
                "Minor Conduit",
                500_026,
                25_000_000,
                100,
                48_126,
                vec![48_088],
            ),
        ]
    }
}

/// Resolves extra Drifter Hive completion loot (`Antikythera Element` `34_575` + Sleeper blue loot).
pub fn allocate_drifter_hive_bonus_loot(
    reg: &mut PveSpecialSitesRegistry,
    elem_tid: TypeId,
) -> Vec<(i64, TypeId, i32)> {
    let elem_iid = 1_899_000_000 + next_spec_seq(reg);
    let blue_iid = 1_899_100_000 + next_spec_seq(reg);
    let antikythera_iid = 1_899_200_000 + next_spec_seq(reg);
    vec![
        (elem_iid, elem_tid, 1),
        (antikythera_iid, 34_575, 2),
        (blue_iid, 30_745, 5),
    ]
}
