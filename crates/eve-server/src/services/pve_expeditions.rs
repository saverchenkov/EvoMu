//! `pve_expeditions` — Multi-step PvE Escalations and Expeditions (`feat:pve.escalations`, `feat:pve.expeditions`).
//!
//! Provides dynamic multi-step expedition progression in character journal/state with dynamic steps,
//! spawning of destination dungeon instances in destination solar systems, and step progression
//! when destination encounters are defeated.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

use crate::router::ServerContext;
use eve_machonet::SessionState;
use crate::services::pve_exploration::{
    build_wave_npc_balls, next_pve_seq, now_sec,
    PveExplorationRegistry, PveSiteInstance,
};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SolarSystemId;

pub type CharacterId = i64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalatingPathEntry {
    pub instance_id: i64,
    pub character_id: CharacterId,
    pub source_dungeon_id: u32,
    pub expedition_dungeon_id: u32,
    pub expedition_name: String,
    pub destination_solar_system_id: SolarSystemId,
    pub step_index: u8,
    pub total_steps: u8,
    pub journal_text: String,
    pub created_at_sec: u64,
    pub expires_at_sec: u64,
    pub dungeons_entered: Vec<u32>,
}

pub fn find_escalation_destination_system_sde(sde: &eve_sde::SdeRegistry, origin_sys_id: SolarSystemId) -> SolarSystemId {
    let mut candidate = origin_sys_id;
    let adj_systems: Vec<u32> = sde
        .jumps
        .values()
        .filter(|j| j.from_solar_system_id == origin_sys_id)
        .map(|j| j.to_solar_system_id)
        .collect();
    if let Some(&next) = adj_systems.first() {
        candidate = next;
    }
    if candidate == origin_sys_id {
        candidate = match origin_sys_id {
            30_000_142 => 30_000_144, // Jita -> Perimeter
            30_000_144 => 30_000_145, // Perimeter -> Urlen
            30_002_187 => 30_002_188, // Amarr
            30_002_659 => 30_002_660, // Dodixie
            30_002_510 => 30_002_511, // Rens
            other => (other + 1).max(30_000_001),
        };
    }
    candidate
}

pub fn find_escalation_destination_system(ctx: &ServerContext, origin_sys_id: SolarSystemId) -> SolarSystemId {
    find_escalation_destination_system_sde(&ctx.sde, origin_sys_id)
}

pub fn expedition_dungeon_chain(source_dungeon_id: u32) -> (u32, &'static str, u8) {
    match source_dungeon_id {
        // Guristas
        1420 => (1420, "Guristas Scout Outpost", 3),
        1215 | 1421 => (1421, "Guristas Hallucinogen Supply Waypoint", 3),
        1232 | 1422 => (1422, "Chasing the Dragon", 4),
        2283 | 2402 => (2402, "Guristas Fleet Staging Point", 4),
        2134 | 2321 | 2403 => (2403, "The Maze", 4),

        // Blood Raiders
        2141 | 1430 => (1430, "Blood Raider Scout Outpost", 3),
        1218 | 1431 => (1431, "Blood Raider Crimson Hand Supply Depot", 3),
        1229 | 1432 => (1432, "Blood Raider Fleet Staging Point", 4),
        2282 | 2410 => (2410, "Blood Raider Fleet Staging Point 2", 4),
        2319 | 2411 => (2411, "Crimson Gauntlet", 4),

        // Sansha's Nation
        2135 | 1440 => (1440, "Sansha Scout Outpost", 3),
        1216 | 1441 => (1441, "Sansha Supply Yard", 3),
        1230 | 1442 => (1442, "Sansha Fleet Staging Point", 4),
        2290 | 2420 => (2420, "Centus Assembly Complex", 4),
        2327 | 2421 => (2421, "True Creations Research Facility", 4),

        // Serpentis
        2140 | 1450 => (1450, "Serpentis Scout Outpost", 3),
        1217 | 1451 => (1451, "Serpentis Supply Depot", 3),
        1231 | 1452 => (1452, "Serpentis Fleet Staging Point", 4),
        2300 | 2430 => (2430, "Serpentis Fleet Staging Point 2", 4),
        2330 | 2431 => (2431, "Serpentis Corporation Hydroponics Site", 4),

        // Angel Cartel
        2132 | 1410 => (1410, "Angel Scout Outpost", 3),
        1214 | 1411 => (1411, "Angel Cartel Supply Waypoint", 3),
        1228 | 1412 => (1412, "Angel Fleet Staging Point", 4),
        2115 | 2525 => (2525, "Angel Domination Fleet Staging Point 2.", 4),
        2311 | 2396 => (2396, "Angel Cartel Naval Shipyard", 4),

        // Fallbacks based on dungeon ID ranges
        d if d < 1500 => (1420, "Pirate Scout Outpost", 3),
        d if d < 2200 => (1422, "Pirate Outpost Escalation", 4),
        d if d < 2300 => (2402, "Pirate Fleet Staging Point", 4),
        _ => (2403, "The Maze", 4),
    }
}

pub fn expedition_dungeon_for_step(base_dungeon_id: u32, step_index: u8) -> u32 {
    base_dungeon_id + (step_index.saturating_sub(1) as u32)
}

pub fn roll_escalation_chance(ctx: &ServerContext) -> bool {
    // DERIVED: 20% escalation roll on completion of combat anomaly/signature/DED complex.
    ctx.mechanic_rng.roll(0.20)
}

pub async fn create_escalation_expedition_core(
    db: &std::sync::Arc<eve_db::WorldRepository>,
    sde: &std::sync::Arc<eve_sde::SdeRegistry>,
    systems: &std::sync::Arc<crate::system_actor::SystemRegistry>,
    char_id: CharacterId,
    solar_system_id: SolarSystemId,
    source_dungeon_id: u32,
    dungeon_name: &str,
    pos: [f64; 3],
) -> Option<EscalatingPathEntry> {
    let (exp_base_did, exp_default_name, total_steps) = expedition_dungeon_chain(source_dungeon_id);
    let exp_name = sde
        .dungeons
        .get(&exp_base_did)
        .map(|d| d.name.trim().to_string())
        .unwrap_or_else(|| exp_default_name.to_string());
    let dest_sys_id = find_escalation_destination_system_sde(sde, solar_system_id);

    let (entry, _created_ts) = crate::services::pve_exploration::with_pve_exploration_reg_db(db, db.clock().now_unix_secs(), |reg| {
        let ts = now_sec(reg);
        let inst_id = 910_000 + next_pve_seq(reg);
        let e = EscalatingPathEntry {
            instance_id: inst_id,
            character_id: char_id,
            source_dungeon_id,
            expedition_dungeon_id: exp_base_did,
            expedition_name: exp_name.clone(),
            destination_solar_system_id: dest_sys_id,
            step_index: 1,
            total_steps,
            journal_text: format!(
                "Logs recovered from {dungeon_name} point to coordinates for '{exp_name}' (Step 1/{total_steps}) in solar system {dest_sys_id}."
            ),
            created_at_sec: ts,
            expires_at_sec: ts + 86_400,
            dungeons_entered: vec![source_dungeon_id],
        };
        reg.escalations_by_char.entry(char_id).or_default().push(e.clone());
        (e, ts)
    });

    db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let bm_id = snap.next_entity_id;
        snap.bookmarks.insert(
            bm_id,
            eve_db::BookmarkRecord {
                bookmark_id: bm_id,
                owner_id: char_id,
                label: format!("Escalation: {} [Step 1/{total_steps}]", entry.expedition_name),
                solar_system_id: dest_sys_id,
                x: pos[0],
                y: pos[1],
                z: pos[2],
                item_id: Some(entry.instance_id),
                memo: entry.journal_text.clone(),
                folder_id: None,
                is_shared: false,
            },
        );
    });

    spawn_expedition_site_instance_core(sde, systems, db, &entry).await;
    Some(entry)
}

pub async fn create_escalation_expedition(
    ctx: &ServerContext,
    char_id: CharacterId,
    solar_system_id: SolarSystemId,
    source_dungeon_id: u32,
    dungeon_name: &str,
    pos: [f64; 3],
) -> Option<EscalatingPathEntry> {
    let entry = create_escalation_expedition_core(&ctx.db, &ctx.sde, &ctx.systems, char_id, solar_system_id, source_dungeon_id, dungeon_name, pos).await?;
    let exp_pos = [
        -45_000_000.0 + (entry.step_index as f64) * 5_000_000.0,
        15_000_000.0,
        30_000_000.0,
    ];
    let _ = crate::services::pve_dungeon_runtime::spawn_sde_dungeon_runtime_instance(
        ctx,
        char_id,
        entry.destination_solar_system_id,
        entry.expedition_dungeon_id,
        "Expedition",
        exp_pos,
        false,
    )
    .await;
    crate::services::notify_emitters_batch3_space::emit_escalation_created_or_advanced(
        ctx,
        char_id,
        entry.instance_id,
        entry.expedition_dungeon_id,
        true,
    );
    Some(entry)
}

pub async fn spawn_expedition_site_instance_core(
    sde: &std::sync::Arc<eve_sde::SdeRegistry>,
    systems: &std::sync::Arc<crate::system_actor::SystemRegistry>,
    db: &std::sync::Arc<eve_db::WorldRepository>,
    entry: &EscalatingPathEntry,
) {
    let dest_sys = entry.destination_solar_system_id;
    let site_id = format!("EXP-{}-{}", entry.instance_id, entry.step_index);
    let pos = [
        -45_000_000.0 + (entry.step_index as f64) * 5_000_000.0,
        15_000_000.0,
        30_000_000.0,
    ];
    let initial_balls = build_wave_npc_balls(sde, dest_sys, entry.expedition_dungeon_id, 1, pos, None);
    let mut spawned_ids = Vec::new();
    let sys_handle = systems.get_or_spawn(dest_sys);
    for b in initial_balls {
        spawned_ids.push(b.id);
        let cs = crate::services::pve_exploration_seeding::build_type_combat_state_from_sde(sde, b.type_id);
        let _ = sys_handle.spawn_ball_with_combat(b, cs).await;
    }

    crate::services::pve_exploration::with_pve_exploration_reg_db(db, db.clock().now_unix_secs(), |reg| {
        let sites = reg.sites_by_system.entry(dest_sys).or_default();
        sites.retain(|s| s.site_id != site_id);
        sites.push(PveSiteInstance {
            site_id,
            dungeon_id: entry.expedition_dungeon_id,
            dungeon_name: format!(
                "{} [Step {}/{}]",
                entry.expedition_name, entry.step_index, entry.total_steps
            ),
            solar_system_id: dest_sys,
            site_category: "Expedition".into(),
            ded_rating: None,
            wormhole_class: None,
            position: pos,
            probe_required: false,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: 2,
            active_npc_ball_ids: spawned_ids,
            hackable_can_ids: Vec::new(),
            npc_guards_spawned: true,
            ghost_timer_started_at: None,
            ghost_detonate_after_sec: 0,
            ghost_detonated: false,
            sleeper_rift_stabilized: true,
            sleeper_sentry_disabled: true,
            sleeper_gas_cloud_active: false,
            cleared: false,
            created_unix: entry.created_at_sec,
            lifetime_seconds: entry.expires_at_sec.saturating_sub(entry.created_at_sec),
            expedition_instance_id: Some(entry.instance_id),
            gate_ball_id: None,
            ..Default::default()
        });
    });
}

pub async fn spawn_expedition_site_instance(
    ctx: &ServerContext,
    entry: &EscalatingPathEntry,
) {
    spawn_expedition_site_instance_core(&ctx.sde, &ctx.systems, &ctx.db, entry).await;
}

pub async fn advance_expedition_step_core(
    db: &std::sync::Arc<eve_db::WorldRepository>,
    sde: &std::sync::Arc<eve_sde::SdeRegistry>,
    systems: &std::sync::Arc<crate::system_actor::SystemRegistry>,
    char_id: CharacterId,
    instance_id: i64,
) -> Result<Option<EscalatingPathEntry>, BridgeRpcError> {
    let mut updated_entry = None;
    let mut completed = false;

    crate::services::pve_exploration::with_pve_exploration_reg_db(db, db.clock().now_unix_secs(), |reg| {
        let ts = now_sec(reg);
        let Some(list) = reg.escalations_by_char.get_mut(&char_id) else {
            return;
        };
        let Some(entry) = list.iter_mut().find(|e| e.instance_id == instance_id) else {
            return;
        };

        entry.dungeons_entered.push(entry.expedition_dungeon_id);

        if entry.step_index < entry.total_steps {
            entry.step_index += 1;
            let next_dest = find_escalation_destination_system_sde(sde, entry.destination_solar_system_id);
            entry.destination_solar_system_id = next_dest;
            let (exp_base_did, _, _) = expedition_dungeon_chain(entry.source_dungeon_id);
            entry.expedition_dungeon_id = expedition_dungeon_for_step(exp_base_did, entry.step_index);
            entry.expires_at_sec = ts + 86_400;
            entry.journal_text = format!(
                "Intelligence confirmed at previous encounter. Target coordinates updated for '{}' (Step {}/{}) in solar system {}.",
                entry.expedition_name, entry.step_index, entry.total_steps, next_dest
            );
            updated_entry = Some(entry.clone());
        } else {
            completed = true;
            let mut done_entry = entry.clone();
            done_entry.journal_text = format!(
                "Expedition '{}' has been fully completed! All pirate overseers neutralized.",
                done_entry.expedition_name
            );
            list.retain(|e| e.instance_id != instance_id);
            updated_entry = Some(done_entry);
        }
    });

    if let Some(ref entry) = updated_entry {
        if !completed {
            spawn_expedition_site_instance_core(sde, systems, db, entry).await;

            db.with_mut_snapshot(|snap| {
                if let Some(bm) = snap
                    .bookmarks
                    .values_mut()
                    .find(|b| b.owner_id == char_id && b.item_id == Some(instance_id))
                {
                    bm.solar_system_id = entry.destination_solar_system_id;
                    bm.label = format!(
                        "Escalation: {} [Step {}/{}]",
                        entry.expedition_name, entry.step_index, entry.total_steps
                    );
                    bm.memo = entry.journal_text.clone();
                }
            });
        } else {
            let dest_sys = entry.destination_solar_system_id;
            let pos = [-45_000_000.0, 15_000_000.0, 30_000_000.0];
            crate::services::pve_exploration::drop_site_loot_into_space_core(db, systems, dest_sys, char_id, pos, 19419, 1).await;

            db.with_mut_snapshot(|snap| {
                snap.bookmarks.retain(|_, b| !(b.owner_id == char_id && b.item_id == Some(instance_id)));
            });
        }
    }

    Ok(updated_entry)
}

pub async fn advance_expedition_step(
    ctx: &ServerContext,
    char_id: CharacterId,
    instance_id: i64,
) -> Result<Option<EscalatingPathEntry>, BridgeRpcError> {
    let updated = advance_expedition_step_core(&ctx.db, &ctx.sde, &ctx.systems, char_id, instance_id).await?;
    if let Some(ref entry) = updated {
        crate::services::notify_emitters_batch3_space::emit_escalation_created_or_advanced(
            ctx,
            char_id,
            entry.instance_id,
            entry.expedition_dungeon_id,
            true,
        );
    }
    Ok(updated)
}

fn dungeon_kv_for_id(sde: &eve_sde::SdeRegistry, dungeon_id: u32, fallback_name: &str) -> PyRep {
    let (name, faction_id, archetype_id) = sde
        .dungeons
        .get(&dungeon_id)
        .map(|d| {
            (
                d.name.clone(),
                d.faction_id.map(|f| PyRep::Integer(i64::from(f))).unwrap_or(PyRep::None),
                d.archetype_id.map(|a| PyRep::Integer(i64::from(a))).unwrap_or(PyRep::None),
            )
        })
        .unwrap_or_else(|| (fallback_name.to_string(), PyRep::None, PyRep::None));
    PyRep::keyval([
        ("dungeonID", PyRep::Integer(i64::from(dungeon_id))),
        ("dungeonName", PyRep::String(name)),
        ("dungeonNameID", PyRep::None),
        ("factionID", faction_id),
        ("archetypeID", archetype_id),
    ])
}

fn dungeon_instance_kv_for_id(
    sde: &eve_sde::SdeRegistry,
    instance_id: i64,
    solar_system_id: SolarSystemId,
    dungeon_id: u32,
    fallback_name: &str,
) -> PyRep {
    let (name, faction_id, archetype_id) = sde
        .dungeons
        .get(&dungeon_id)
        .map(|d| {
            (
                d.name.clone(),
                d.faction_id.map(|f| PyRep::Integer(i64::from(f))).unwrap_or(PyRep::None),
                d.archetype_id.map(|a| PyRep::Integer(i64::from(a))).unwrap_or(PyRep::None),
            )
        })
        .unwrap_or_else(|| (fallback_name.to_string(), PyRep::None, PyRep::None));
    PyRep::keyval([
        ("instanceID", PyRep::Integer(instance_id)),
        ("solarSystemID", PyRep::Integer(i64::from(solar_system_id))),
        ("dungeonID", PyRep::Integer(i64::from(dungeon_id))),
        ("dungeonName", PyRep::String(name)),
        ("dungeonNameID", PyRep::None),
        ("factionID", faction_id),
        ("archetypeID", archetype_id),
    ])
}

pub fn get_my_escalating_path_details_rowset(ctx: &ServerContext, char_id: CharacterId) -> PyRep {
    let (active_entries, expired_instance_ids) =
        crate::services::pve_exploration::with_pve_exploration_reg(ctx, |reg| {
            let now = now_sec(reg);
            let mut expired_ids = Vec::new();
            let active = if let Some(list) = reg.escalations_by_char.get_mut(&char_id) {
                let mut kept = Vec::new();
                for e in list.drain(..) {
                    if now < e.expires_at_sec {
                        kept.push(e);
                    } else {
                        expired_ids.push(e.instance_id);
                    }
                }
                *list = kept.clone();
                kept
            } else {
                Vec::new()
            };
            (active, expired_ids)
        });

    if !expired_instance_ids.is_empty() {
        ctx.db.with_mut_snapshot(|snap| {
            snap.bookmarks.retain(|_, b| {
                !(b.owner_id == char_id
                    && b.item_id
                        .is_some_and(|iid| expired_instance_ids.contains(&iid)))
            });
        });
        ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnEscalatingPathChange",
            "charid",
            vec![],
        );
    }

    let columns = vec![
        "instanceID",
        "sourceDungeonID",
        "dungeonID",
        "dungeonName",
        "dungeonNameID",
        "destinationSolarSystemID",
        "solarSystemID",
        "stepIndex",
        "totalSteps",
        "journalText",
        "createdAt",
        "expiresAt",
        "expiryTime",
        "journalEntryTemplateID",
        "dungeon",
        "destDungeon",
        "srcDungeon",
    ];

    let rows: Vec<Vec<PyRep>> = active_entries
        .into_iter()
        .map(|e| {
            let expiry_ft = ((e.expires_at_sec as i64) + 11_644_473_600) * 10_000_000;
            let dungeon_kv = dungeon_instance_kv_for_id(
                &ctx.sde,
                e.instance_id,
                e.destination_solar_system_id,
                e.expedition_dungeon_id,
                &e.expedition_name,
            );
            let dest_kv = dungeon_kv_for_id(&ctx.sde, e.expedition_dungeon_id, &e.expedition_name);
            let src_kv = dungeon_kv_for_id(&ctx.sde, e.source_dungeon_id, &e.expedition_name);
            vec![
                PyRep::Integer(e.instance_id),
                PyRep::Integer(i64::from(e.source_dungeon_id)),
                PyRep::Integer(i64::from(e.expedition_dungeon_id)),
                PyRep::String(e.expedition_name),
                PyRep::None,
                PyRep::Integer(i64::from(e.destination_solar_system_id)),
                PyRep::Integer(i64::from(e.destination_solar_system_id)),
                PyRep::Integer(i64::from(e.step_index)),
                PyRep::Integer(i64::from(e.total_steps)),
                PyRep::String(e.journal_text),
                PyRep::Integer(e.created_at_sec as i64),
                PyRep::Integer(e.expires_at_sec as i64),
                PyRep::Integer(expiry_ft),
                PyRep::Integer(i64::from(e.step_index)),
                dungeon_kv,
                dest_kv,
                src_kv,
            ]
        })
        .collect();

    PyRep::rowset(columns, rows)
}

pub fn get_escalating_path_dungeons_entered(reg: &PveExplorationRegistry, char_id: CharacterId) -> Vec<PyRep> {
    reg.escalations_by_char
        .get(&char_id)
        .map(|list| {
            list.iter()
                .flat_map(|e| e.dungeons_entered.iter().copied())
                .map(|d| PyRep::Integer(d as i64))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub async fn validate_and_advance_pve_site_room(
    ctx: &ServerContext,
    session: &mut SessionState,
    sys_id: SolarSystemId,
    raw_site_id: &str,
) -> Result<PyRep, BridgeRpcError> {
    let mut site_id = raw_site_id.to_string();
    let (rating_to_check, sleeper_info, site_pos_opt, canonical_site_id) =
        crate::services::pve_exploration::with_pve_exploration_reg(ctx, |reg| {
            let site = reg.sites_by_system.get(&sys_id).and_then(|l| {
                l.iter()
                    .find(|s| s.site_id == site_id || s.dungeon_id.to_string() == site_id)
            });
            let r = site.and_then(|s| s.ded_rating);
            let s_info = site.and_then(|s| {
                if s.site_category == "SleeperCache" {
                    let tier = crate::services::pve_sleeper_caches::sleeper_cache_tier_for_dungeon(
                        s.dungeon_id,
                    )
                    .unwrap_or(crate::services::pve_sleeper_caches::SleeperCacheTier::Standard);
                    Some((tier, s.sleeper_rift_stabilized, s.clone()))
                } else {
                    None
                }
            });
            let pos = site.map(|s| s.position);
            (r, s_info, pos, site.map(|s| s.site_id.clone()))
        });
    if let Some(cid) = canonical_site_id {
        site_id = cid;
    }

    if let Some(rating) = rating_to_check {
        let ship = session.ship_id.and_then(|sid| {
            ctx.db.with_snapshot(|snap| {
                snap.items.get(&sid).and_then(|item| {
                    ctx.sde
                        .types
                        .get(&item.type_id)
                        .map(|t| (item.type_id, t.mass))
                })
            })
        });
        let Some((ship_tid, mass)) = ship else {
            return Err(BridgeRpcError {
                code: "ShipNotFound".into(),
                message: "Acceleration gate activation requires an active ship item with an SDE hull type".into(),
            });
        };
        crate::services::pve_exploration_sites::check_ded_gate_ship_allowed(
            &ctx.sde, rating, ship_tid, mass,
        )?;
    }

    if let (Some(site_pos), Some(_)) = (site_pos_opt, rating_to_check) {
        let ship_pos = match session.ship_id {
            Some(sid) => ctx
                .systems
                .get_or_spawn(sys_id)
                .get_ball_snapshot(sid)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position),
            None => None,
        };
        let Some(ship_pos) = ship_pos else {
            return Err(BridgeRpcError {
                code: "NotInSpace".into(),
                message: "Acceleration gate activation requires the pilot's ship in space".into(),
            });
        };
        let dist = ship_pos.distance(glam::DVec3::from_array(site_pos));
        if dist > 2_500.0 {
            return Err(BridgeRpcError {
                code: "GateOutOfRange".into(),
                message: format!(
                    "Pilot is {dist:.1}m from acceleration gate; must be within 2,500m to activate"
                ),
            });
        }
    }

    if let Some((tier, stabilized, site_clone)) = sleeper_info {
        let (grp, mass) = session
            .ship_id
            .and_then(|sid| {
                ctx.db.with_snapshot(|snap| {
                    snap.items
                        .get(&sid)
                        .and_then(|item| ctx.sde.types.get(&item.type_id))
                        .map(|t| (t.group_id, t.mass))
                })
            })
            .unwrap_or((25, 1_000_000.0));
        crate::services::pve_sleeper_caches::validate_sleeper_cache_passage(
            tier, stabilized, grp, mass,
        )?;
        crate::services::pve_sleeper_caches::apply_sleeper_hazard_damage_if_active(
            ctx,
            session,
            sys_id,
            &site_clone,
        )
        .await;
    }

    let out = crate::services::pve_exploration::advance_site_wave_or_complete(
        ctx, session, sys_id, &site_id,
    )
    .await?;

    if let Some(char_id) = session.character_id {
        let info = crate::services::pve_exploration::with_pve_exploration_reg(ctx, |reg| {
            reg.sites_by_system
                .get(&sys_id)
                .and_then(|l| l.iter().find(|s| s.site_id == site_id))
                .map(|s| {
                    (
                        s.dungeon_id,
                        s.site_category.clone(),
                        s.dungeon_name.clone(),
                        s.position,
                        s.current_wave,
                        s.total_waves,
                        s.cleared,
                        s.created_unix,
                        s.lifetime_seconds,
                        s.expedition_instance_id,
                    )
                })
        });
        if let Some((did, cat, dname, spos, cw, tw, clr, cu, ls, exp_inst)) = info {
            let life = if ls == 0 { 86_400 } else { ls };
            let exp_ft = ((cu.saturating_add(life) as i64) + 11_644_473_600) * 10_000_000;
            let fallback_room_id = i64::from(did.max(1)) * 100 + i64::from(cw);
            crate::services::notify_emitters_batch3_space::emit_dungeon_progression_notifications(
                ctx,
                char_id,
                sys_id,
                &cat,
                &dname,
                i64::from(did),
                exp_inst.unwrap_or(fallback_room_id),
                fallback_room_id,
                spos,
                cw,
                tw,
                clr,
                exp_ft,
            );
        }
    }
    Ok(out)
}

pub fn enter_combat_anomaly_site(
    ctx: &ServerContext,
    char_id: CharacterId,
    sys_id: SolarSystemId,
    site_id: &str,
) -> Result<PyRep, BridgeRpcError> {
    let site = crate::services::pve_exploration::with_pve_exploration_reg(ctx, |reg| {
        let Some(site) = reg
            .sites_by_system
            .get(&sys_id)
            .and_then(|l| l.iter().find(|s| s.site_id == site_id))
            .cloned()
        else {
            return Err(BridgeRpcError {
                code: "AnomalyNotFound".into(),
                message: format!("Combat anomaly {site_id} not found"),
            });
        };
        if let Some(list) = reg.escalations_by_char.get_mut(&char_id) {
            for esc in list.iter_mut() {
                if (site.expedition_instance_id == Some(esc.instance_id)
                    || (esc.destination_solar_system_id == sys_id
                        && esc.expedition_dungeon_id == site.dungeon_id))
                    && !esc.dungeons_entered.contains(&site.dungeon_id)
                {
                    esc.dungeons_entered.push(site.dungeon_id);
                }
            }
        }
        Ok(site)
    })?;

    let life = if site.lifetime_seconds == 0 {
        86_400
    } else {
        site.lifetime_seconds
    };
    let exp_ft = ((site.created_unix.saturating_add(life) as i64) + 11_644_473_600) * 10_000_000;
    let fallback_room_id = i64::from(site.dungeon_id.max(1)) * 100 + i64::from(site.current_wave);
    crate::services::notify_emitters_batch3_space::emit_dungeon_progression_notifications(
        ctx,
        char_id,
        sys_id,
        &site.site_category,
        &site.dungeon_name,
        i64::from(site.dungeon_id),
        site.expedition_instance_id.unwrap_or(fallback_room_id),
        fallback_room_id,
        site.position,
        site.current_wave,
        site.total_waves,
        site.cleared,
        exp_ft,
    );

    Ok(PyRep::keyval([
        ("siteID", PyRep::String(site.site_id)),
        ("dungeonID", PyRep::Integer(site.dungeon_id as i64)),
        ("dungeonName", PyRep::String(site.dungeon_name)),
        ("currentWave", PyRep::Integer(site.current_wave as i64)),
        ("totalWaves", PyRep::Integer(site.total_waves as i64)),
        (
            "activeNpcBallIDs",
            PyRep::List(
                site.active_npc_ball_ids
                    .into_iter()
                    .map(PyRep::Integer)
                    .collect(),
            ),
        ),
    ]))
}
