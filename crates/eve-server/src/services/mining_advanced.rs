//! `mining_advanced` — Advanced Mining Systems (`MN-13`..`MN-18`).
//!
//! Implements:
//! - `MN-13`: Moon extraction lifecycle (`moonExtractions.*`): StartNewExtraction, CancelExtraction,
//!   FractureChunkWithStructure, GetExtractionsForCorp, GetMoonResourcesForStructure,
//!   GetExtractionAndEventIDFromStructureID, and chunk explosion into minable moon asteroids.
//! - `MN-14`: Auto moon mining service (`autoMoonMining.*`): RequestMiningDetails, RequestMiningCycleOutput
//!   (Metenox Moon Drill 40% efficiency, fuel consumption, and output bay deposits).
//! - `MN-15`: Corp mining ledger service (`corpMiningLedger.*`): GetObserverLedger, GetObserversWithMiningEvents,
//!   and observer volume mining tracking.
//! - `MN-16`: In-space and structure compression & decompression (`inSpaceCompressionMgr.*`,
//!   `structureCompressionMgr.*`): CompressItemInSpace, CompressItemInStructure, DecompressGasInStructure,
//!   GetMyGasDecompressionEfficiency.
//! - `MN-18`: Career goal mining progression verified strictly from mining ledger entries.

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_db::{
    AutoMoonMinerRecord, InventoryFlag, InventoryItemRecord, MoonExtractionRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SdeRegistry, TypeId};
use glam::DVec3;
use std::collections::HashMap;

/// Resolves the nearest SDE moon celestial (`group_id == 8`) in `solar_system_id` to `pos`.
pub(crate) fn resolve_structure_moon_id(
    sde: &eve_sde::SdeRegistry,
    solar_system_id: u32,
    pos: [f64; 3],
) -> Option<i64> {
    sde.celestials_in_system(solar_system_id)
        .into_iter()
        .filter(|c| c.group_id == 8)
        .min_by(|a, b| {
            let da = (a.position.x - pos[0]).powi(2)
                + (a.position.y - pos[1]).powi(2)
                + (a.position.z - pos[2]).powi(2);
            let db = (b.position.x - pos[0]).powi(2)
                + (b.position.y - pos[1]).powi(2)
                + (b.position.z - pos[2]).powi(2);
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|c| c.celestial_id)
}

/// Deterministically derives a 3-ore moon composition from SDE moon ore groups (`1884`, `1920..=1923`)
/// keyed by `solar_system_id` security band and the SDE `moon_id`.
/// High-Sec (`sec >= 0.45`) and Wormhole (`solar_system_id >= 31_000_000`) moons only yield R4 Ubiquitous Moon Ores (`1884`),
/// while Low-Sec and Null-Sec moons yield R4..R64 (`1884`, `1920..=1923`) per `docs/eve-wiki/Moon_mining.md:95-104`.
/// DERIVED: CCP's public SDE does not contain Tranquility moon survey tables; ore types are selected
/// from published SDE moon ore types seeded by `moon_id` and security band.
pub(crate) fn derive_moon_ores_from_sde(
    sde: &eve_sde::SdeRegistry,
    solar_system_id: u32,
    moon_id: i64,
) -> Vec<(TypeId, f64)> {
    let sec = sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    let restrict_to_r4 = sec >= 0.45 || solar_system_id >= 31_000_000;
    let is_base_moon_ore = |t: &eve_sde::InvType| -> bool {
        t.published && !["Compressed", "Brimful", "Bountiful", "Shining", "Glistening", "Glowing", "Twinkling", "Shimmering", "Resplendent", "Sparkling", "Dazzling", "Lavish"].iter().any(|p| t.name.contains(p))
    };
    let mut base_moon_ores: Vec<TypeId> = sde.types.values()
        .filter(|t| (if restrict_to_r4 { t.group_id == 1884 } else { matches!(t.group_id, 1884 | 1920..=1923) }) && is_base_moon_ore(t))
        .map(|t| t.type_id).collect();
    base_moon_ores.sort_unstable();
    if base_moon_ores.len() < 3 {
        return vec![(45492, 0.40), (45490, 0.35), (45494, 0.25)];
    }
    let h = (moon_id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut idx0 = (h as usize) % base_moon_ores.len();
    if !restrict_to_r4 {
        let higher_ores: Vec<usize> = base_moon_ores.iter().enumerate()
            .filter(|(_, &tid)| sde.types.get(&tid).is_some_and(|t| matches!(t.group_id, 1920..=1923)))
            .map(|(i, _)| i).collect();
        if !higher_ores.is_empty() {
            idx0 = higher_ores[(h as usize) % higher_ores.len()];
        }
    }
    let idx1 = ((h >> 16) as usize % (base_moon_ores.len() - 1) + idx0 + 1) % base_moon_ores.len();
    let mut idx2 = ((h >> 32) as usize) % base_moon_ores.len();
    while idx2 == idx0 || idx2 == idx1 {
        idx2 = (idx2 + 1) % base_moon_ores.len();
    }
    let w0 = 35 + ((h >> 40) % 15) as u32;
    let w1 = 25 + ((h >> 48) % 15) as u32;
    let w2 = 100u32.saturating_sub(w0 + w1);
    vec![
        (base_moon_ores[idx0], (w0 as f64) / 100.0),
        (base_moon_ores[idx1], (w1 as f64) / 100.0),
        (base_moon_ores[idx2], (w2 as f64) / 100.0),
    ]
}

/// Maps a standard moon ore `type_id` to its +15% improved variant (`Brimful`/`Bountiful`/`Replete`/`Copious`/`Lavish`) in the same SDE group.
fn find_improved_variant(sde: &eve_sde::SdeRegistry, base_ore_tid: TypeId) -> TypeId {
    let Some(base_t) = sde.types.get(&base_ore_tid) else {
        return base_ore_tid;
    };
    sde.types
        .values()
        .find(|t| {
            t.published
                && t.group_id == base_t.group_id
                && t.type_id != base_ore_tid
                && !t.name.contains("Compressed")
                && t.name.ends_with(&base_t.name)
                && (t.name.starts_with("Brimful ")
                    || t.name.starts_with("Bountiful ")
                    || t.name.starts_with("Replete ")
                    || t.name.starts_with("Copious ")
                    || t.name.starts_with("Lavish "))
        })
        .map(|t| t.type_id)
        .unwrap_or(base_ore_tid)
}

/// Maps a standard moon ore `type_id` to its +100% jackpot variant in the same SDE group if present.
fn find_jackpot_variant(sde: &eve_sde::SdeRegistry, base_ore_tid: TypeId) -> TypeId {
    let Some(base_t) = sde.types.get(&base_ore_tid) else {
        return base_ore_tid;
    };
    sde.types
        .values()
        .find(|t| {
            t.published
                && t.group_id == base_t.group_id
                && t.type_id != base_ore_tid
                && !t.name.contains("Compressed")
                && t.name.ends_with(&base_t.name)
                && (t.name.starts_with("Glistening ")
                    || t.name.starts_with("Glowing ")
                    || t.name.starts_with("Twinkling ")
                    || t.name.starts_with("Shimmering ")
                    || t.name.starts_with("Shining ")
                    || t.name.starts_with("Resplendent ")
                    || t.name.starts_with("Sparkling ")
                    || t.name.starts_with("Dazzling "))
        })
        .map(|t| t.type_id)
        .unwrap_or(base_ore_tid)
}

/// Computes the auto-fracture delay after chunk arrival (default 3h = 10,800s, modified by refinery rigs).
fn compute_auto_fracture_delay_secs(
    sde: &eve_sde::SdeRegistry,
    snap: &eve_db::WorldStateSnapshot,
    structure_id: i64,
) -> u64 {
    let mut bonus_pct = 0.0_f64;
    for it in snap.items.values() {
        if it.location_id == structure_id
            && matches!(
                it.flag,
                InventoryFlag::RigSlot0 | InventoryFlag::RigSlot1 | InventoryFlag::RigSlot2
            )
        {
            if let Some(rig_t) = sde.types.get(&it.type_id) {
                let attr_bonus = rig_t
                    .default_attributes
                    .get(&2739)
                    .or_else(|| rig_t.default_attributes.get(&2740))
                    .or_else(|| rig_t.default_attributes.get(&2738))
                    .copied()
                    .unwrap_or(if rig_t.name.contains("Stability") || rig_t.name.contains("Moon") {
                        20.0
                    } else {
                        0.0
                    });
                bonus_pct += attr_bonus.max(0.0);
            }
        }
    }
    (10_800.0 * (1.0 + bonus_pct / 100.0)).round() as u64
}

fn build_moon_chunk_asteroids(
    sde: &eve_sde::SdeRegistry,
    snap: &mut eve_db::WorldStateSnapshot,
    structure_id: i64,
    sys_id: u32,
    st_pos: [f64; 3],
    base_ores: &[(TypeId, f64)],
    total_chunk_volume_m3: f64,
    is_jackpot: bool,
) -> (Vec<i64>, Vec<(i64, TypeId, u64, f64, DVec3)>) {
    let base_pos = DVec3::new(st_pos[0], st_pos[1], st_pos[2]);
    let mut spawned_specs = Vec::new();
    let mut spawned_ids = Vec::new();
    let mut slot_idx = 0usize;

    for &(base_tid, frac) in base_ores {
        let variants: Vec<(TypeId, f64)> = if is_jackpot {
            vec![(find_jackpot_variant(sde, base_tid), frac)]
        } else {
            let imp_tid = find_improved_variant(sde, base_tid);
            vec![(base_tid, frac * 0.5), (imp_tid, frac * 0.5)]
        };
        for (ore_tid, slice_frac) in variants {
            let uvol = sde
                .types
                .get(&ore_tid)
                .map(|t| t.volume.max(0.1))
                .unwrap_or(10.0);
            let units = (((total_chunk_volume_m3 * slice_frac) / uvol).round() as u64).max(1);
            snap.next_entity_id += 1;
            let ast_id = 950_000_000 + snap.next_entity_id;
            let offset = DVec3::new(
                (slot_idx as f64 + 1.0) * 3_500.0,
                (slot_idx as f64) * 2_000.0,
                -(slot_idx as f64 + 1.0) * 3_000.0,
            );
            slot_idx += 1;
            let pos = base_pos + offset;
            snap.asteroids.insert(
                ast_id,
                eve_db::AsteroidRecord {
                    asteroid_id: ast_id,
                    solar_system_id: sys_id,
                    belt_id: structure_id,
                    type_id: ore_tid,
                    quantity: units as i64,
                    initial_quantity: units as i64,
                    position: (pos.x, pos.y, pos.z),
                    radius: 60.0,
                },
            );
            spawned_ids.push(ast_id);
            spawned_specs.push((ast_id, ore_tid, units, uvol, pos));
        }
    }
    (spawned_ids, spawned_specs)
}

async fn spawn_moon_asteroid_balls(
    ctx: &ServerContext,
    sys_id: u32,
    structure_id: i64,
    spawned_specs: Vec<(i64, TypeId, u64, f64, DVec3)>,
) -> Vec<PyRep> {
    let handle = ctx.systems.get_or_spawn(sys_id);
    let mut ore_tids = Vec::with_capacity(spawned_specs.len());
    for (ast_id, ore_tid, units, uvol, pos) in spawned_specs {
        ore_tids.push(PyRep::Integer(ore_tid as i64));
        let ball = Ball {
            id: ast_id, type_id: ore_tid, position: pos, velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 50_000.0, radius: 60.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        };
        let ast_state = crate::system_actor::AsteroidState {
            asteroid_id: ast_id, belt_id: structure_id, type_id: ore_tid,
            initial_units: units, remaining_units: units, unit_volume: uvol,
            depleted: false, respawn_at_tick: None,
        };
        let _ = handle.spawn_asteroid(ast_state, ball).await;
    }
    ore_tids
}

const FRACTURED_FIELDS_BLOB_KEY: &str = "moon_fractured_fields";

fn load_fractured_moon_fields(snap: &eve_db::WorldStateSnapshot) -> Vec<(i64, u32, u64, Vec<i64>)> {
    snap.service_state_blobs
        .get(FRACTURED_FIELDS_BLOB_KEY)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

fn record_fractured_moon_field(
    snap: &mut eve_db::WorldStateSnapshot,
    structure_id: i64,
    sys_id: u32,
    decay_unix: u64,
    asteroid_ids: Vec<i64>,
) {
    if asteroid_ids.is_empty() {
        return;
    }
    let mut fields = load_fractured_moon_fields(snap);
    if !fields.iter().any(|(_, _, d, ids)| *d == decay_unix && *ids == asteroid_ids) {
        fields.push((structure_id, sys_id, decay_unix, asteroid_ids));
        if let Ok(json) = serde_json::to_string(&fields) {
            snap.service_state_blobs.insert(FRACTURED_FIELDS_BLOB_KEY.into(), json);
        }
    }
}

fn resolve_moon_structure_id(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<i64> {
    args.first()
        .and_then(|a| a.as_i64())
        .or_else(|| kwargs.get("structureID").and_then(|v| v.as_i64()))
        .or(session.structure_id)
        .or(session.station_id)
        .or_else(|| {
            session
                .ship_id
                .filter(|sid| ctx.db.with_snapshot(|snap| snap.structures.contains_key(sid)))
        })
        .or_else(|| {
            session.character_id.and_then(|cid| {
                ctx.db.with_snapshot(|snap| {
                    snap.characters
                        .get(&cid)
                        .and_then(|c| c.structure_id.or(c.station_id))
                        .filter(|sid| snap.structures.contains_key(sid))
                })
            })
        })
}

/// Server-driven moon extraction auto-fracture (3h after `chunk_arrival_unix`) and 48h fractured
/// asteroid field decay tick (`plan:MN-13`, `R-MN13-LAZY-LEAK`).
pub async fn tick_all_moon_extractions(ctx: &ServerContext) {
    let now = ctx.now_unix();
    let (decayed_balls, auto_fractures) = ctx.db.with_mut_snapshot(|snap| {
        let mut decayed: Vec<(u32, Vec<i64>)> = Vec::new();
        let mut active_fields = Vec::new();
        for (sid, sys_id, decay_unix, ids) in load_fractured_moon_fields(snap) {
            if now >= decay_unix {
                for &aid in &ids {
                    snap.asteroids.remove(&aid);
                }
                decayed.push((sys_id, ids));
            } else {
                active_fields.push((sid, sys_id, decay_unix, ids));
            }
        }
        if let Ok(json) = serde_json::to_string(&active_fields) {
            snap.service_state_blobs.insert(FRACTURED_FIELDS_BLOB_KEY.into(), json);
        }

        let struct_ids: Vec<i64> = snap.moon_extractions.keys().copied().collect();
        let mut auto_list = Vec::new();
        for structure_id in struct_ids {
            let Some(st) = snap.structures.get(&structure_id) else { continue; };
            let (sys_id, st_pos, owner_corp) = (st.solar_system_id, st.position, st.owner_corp_id);
            let delay_secs = compute_auto_fracture_delay_secs(&ctx.sde, snap, structure_id);
            let Some(ext) = snap.moon_extractions.get(&structure_id).cloned() else { continue; };

            if ext.status != "Cancelled" && now >= ext.natural_decay_unix {
                if let Some(ext_mut) = snap.moon_extractions.get_mut(&structure_id) {
                    let ids = std::mem::take(&mut ext_mut.spawned_asteroid_ids);
                    for &aid in &ids {
                        snap.asteroids.remove(&aid);
                    }
                    ext_mut.status = "Decayed".into();
                    if !ids.is_empty() {
                        decayed.push((sys_id, ids));
                    }
                }
                continue;
            }

            if (ext.status == "Started" || ext.status == "Ready")
                && now >= ext.chunk_arrival_unix.saturating_add(delay_secs)
                && now < ext.natural_decay_unix
            {
                let base_ores = if ext.moon_ores.is_empty() {
                    derive_moon_ores_from_sde(&ctx.sde, sys_id, ext.moon_id)
                } else {
                    ext.moon_ores.clone()
                };
                let (moon_id, start_ts, arr_ts, dec_ts) = (
                    ext.moon_id,
                    ext.extraction_start_unix,
                    ext.chunk_arrival_unix,
                    ext.natural_decay_unix,
                );
                let duration_secs = arr_ts.saturating_sub(start_ts).max(518_400);
                let total_chunk_volume_m3 = (duration_secs as f64 / 3600.0) * 30_000.0;
                let (spawned_ids, spawned_specs) = build_moon_chunk_asteroids(
                    &ctx.sde, snap, structure_id, sys_id, st_pos, &base_ores, total_chunk_volume_m3, false,
                );
                record_fractured_moon_field(snap, structure_id, sys_id, dec_ts, spawned_ids.clone());
                if let Some(ext_mut) = snap.moon_extractions.get_mut(&structure_id) {
                    ext_mut.status = "Fractured".into();
                    ext_mut.auto_fracture = true;
                    ext_mut.spawned_asteroid_ids = spawned_ids;
                }
                let update_rep = PyRep::keyval([
                    ("structureID", PyRep::Integer(structure_id)),
                    ("moonID", PyRep::Integer(moon_id)),
                    ("extractionStartTime", PyRep::Integer(start_ts as i64)),
                    ("chunkArrivalTime", PyRep::Integer(arr_ts as i64)),
                    ("naturalDecayTime", PyRep::Integer(dec_ts as i64)),
                    ("status", PyRep::String("Fractured".into())),
                    ("autoFractured", PyRep::Bool(true)),
                    ("readyToFracture", PyRep::Bool(true)),
                ]);
                auto_list.push((structure_id, sys_id, owner_corp, update_rep, spawned_specs));
            }
        }
        (decayed, auto_list)
    });

    for (sys_id, ids) in decayed_balls {
        let handle = ctx.systems.get_or_spawn(sys_id);
        for aid in ids {
            let _ = handle.remove_ball(aid).await;
        }
    }
    for (structure_id, sys_id, owner_corp, update_rep, spawned_specs) in auto_fractures {
        let _ = spawn_moon_asteroid_balls(ctx, sys_id, structure_id, spawned_specs).await;
        ctx.notify(
            NotificationTarget::Corporation(owner_corp),
            "OnMoonExtractionUpdate",
            "corpid",
            vec![update_rep],
        );
    }
}

#[path = "mining_compression.rs"]
pub mod mining_compression;
pub use mining_compression::*;


pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // ====================================================================
        // MN-13: Moon Extractions Lifecycle (moonExtractions.*)
        // ====================================================================
        ("moonExtractions", "StartNewExtraction") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let sess_struct_id = resolve_moon_structure_id(ctx, session, &[], _kwargs);
            let (structure_id, explicit_dur) = if args.len() >= 2 {
                (args.first().and_then(|a| a.as_i64()), args.get(1).and_then(|a| a.as_i64()))
            } else if let Some(arg0) = args.first().and_then(|a| a.as_i64()) {
                let arg0_is_known_struct = ctx.db.with_snapshot(|snap| snap.structures.contains_key(&arg0));
                if !arg0_is_known_struct && sess_struct_id.is_some() && arg0 < 100_000_000 {
                    (sess_struct_id, Some(arg0))
                } else {
                    (Some(arg0), None)
                }
            } else {
                (
                    sess_struct_id,
                    _kwargs
                        .get("duration")
                        .or_else(|| _kwargs.get("extractionLength"))
                        .and_then(|v| v.as_i64()),
                )
            };
            let Some(structure_id) = structure_id else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "StartNewExtraction requires structureID".into(),
                }));
            };

            // Server-authoritative extraction timings (`docs/eve-wiki/Moon_mining.md:107-112`):
            // duration must be in 6d..=56d (`518_400..=4_838_400` seconds, default 6d = 518,400s),
            // natural decay window fixed at 48h (172,800s) after chunk arrival.
            let now = ctx.now_unix();
            let duration_secs = match explicit_dur {
                Some(raw_dur) => {
                    let secs = if raw_dur > now as i64 {
                        (raw_dur as u64).saturating_sub(now)
                    } else if raw_dur > 0 {
                        raw_dur as u64
                    } else {
                        0
                    };
                    if !(518_400..=4_838_400).contains(&secs) {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidExtractionDuration".into(),
                            message: format!(
                                "Moon extraction duration must be between 6 days (518400s) and 56 days (4838400s), got {secs}s"
                            ),
                        }));
                    }
                    secs
                }
                None => 518_400,
            };
            let start_time = now;
            let arrival_time = start_time + duration_secs;
            let decay_time = arrival_time + 172_800;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(st) = snap.structures.get(&structure_id) else {
                    return Err(BridgeRpcError {
                        code: "StructureNotFound".into(),
                        message: format!("Structure {structure_id} not found"),
                    });
                };

                let caller_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0);
                if st.owner_corp_id != caller_corp
                    || !mining_compression::caller_has_structure_extraction_role(
                        snap,
                        char_id,
                        caller_corp,
                        session.corp_role,
                    )
                {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: format!(
                            "Character {char_id} (corp {caller_corp}) lacks permissions on structure {structure_id} (corp {})",
                            st.owner_corp_id
                        ),
                    });
                }
                if !matches!(st.type_id, 35835 | 35836) {
                    return Err(BridgeRpcError {
                        code: "InvalidStructureType".into(),
                        message: format!(
                            "Moon extraction requires an Athanor (35835) or Tatara (35836) refinery, got structure type {}",
                            st.type_id
                        ),
                    });
                }
                if st.fuel_blocks == 0 || !st.quantum_core_installed {
                    return Err(BridgeRpcError {
                        code: "StructureServiceOffline".into(),
                        message: "Refinery must have a Quantum Core installed and fuel blocks to operate moon drill".into(),
                    });
                }

                let st_sys_id = st.solar_system_id;
                if let Some((prev_decay, prev_ids)) = snap.moon_extractions.get(&structure_id).map(|e| {
                    let is_active = (e.status == "Started" || e.status == "Ready") && now < e.natural_decay_unix;
                    if is_active {
                        return Err(BridgeRpcError {
                            code: "ExtractionAlreadyActive".into(),
                            message: format!("Structure {structure_id} already has an active moon extraction in state {}", e.status),
                        });
                    }
                    Ok((e.natural_decay_unix, e.spawned_asteroid_ids.clone()))
                }).transpose()? {
                    if !prev_ids.is_empty() {
                        record_fractured_moon_field(snap, structure_id, st_sys_id, prev_decay, prev_ids);
                    }
                }
                let st = snap.structures.get(&structure_id).unwrap();

                let Some(moon_id) = resolve_structure_moon_id(&ctx.sde, st.solar_system_id, st.position) else {
                    return Err(BridgeRpcError {
                        code: "NoMoonInSystem".into(),
                        message: format!("No SDE moon celestial found in system {}", st.solar_system_id),
                    });
                };

                let moon_ores = derive_moon_ores_from_sde(&ctx.sde, st.solar_system_id, moon_id);

                let max_cal_id = snap.calendar_events.keys().copied().max().unwrap_or(0);
                snap.next_entity_id = snap.next_entity_id.max(max_cal_id) + 1;
                let event_id = snap.next_entity_id;
                let moon_name = ctx
                    .sde
                    .celestials_in_system(st.solar_system_id)
                    .into_iter()
                    .find(|c| c.celestial_id == moon_id)
                    .map(|c| c.name)
                    .unwrap_or_else(|| format!("Moon {moon_id}"));
                let st_name = st.name.clone();
                let st_corp = st.owner_corp_id;
                let arrival_ft = (arrival_time as i64 + 11_644_473_600) * 10_000_000;
                // DERIVED: automated calendar event duration in minutes equals the natural decay window (`ASTEROID_DECAY_DURATION = 172_800s = 2880m`).
                let decay_mins = ((decay_time.saturating_sub(arrival_time)) / 60) as i64;
                let ev_title = format!("Moon Extraction Ready: {moon_name} ({st_name})");
                let ev_desc = format!(
                    "Automated moon extraction chunk arrival at {st_name} orbiting {moon_name}"
                );
                snap.calendar_events.insert(
                    event_id,
                    eve_db::CalendarEventRecord {
                        event_id,
                        owner_id: st_corp,
                        creator_id: char_id,
                        event_date_time: arrival_ft,
                        duration: decay_mins,
                        importance: 1,
                        title: ev_title.clone(),
                        description: ev_desc.clone(),
                        scope: "Automated".into(),
                        responses: HashMap::new(),
                        is_deleted: false,
                    },
                );

                let record = MoonExtractionRecord {
                    structure_id,
                    moon_id,
                    extraction_start_unix: start_time,
                    chunk_arrival_unix: arrival_time,
                    natural_decay_unix: decay_time,
                    status: "Started".into(),
                    auto_fracture: false,
                    event_id,
                    moon_ores: moon_ores.clone(),
                    spawned_asteroid_ids: Vec::new(),
                };

                snap.moon_extractions.insert(structure_id, record);
                Ok((structure_id, moon_id, start_time, arrival_time, decay_time, event_id, moon_ores, st_corp, arrival_ft, decay_mins, ev_title, ev_desc))
            });

            match res {
                Ok((s_id, m_id, st, arr, dec, ev_id, ores, st_corp, arrival_ft, decay_mins, ev_title, ev_desc)) => {
                    {
                        let mut s = ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner());
                        s.next_id = s.next_id.max(ev_id);
                        s.calendar_events.insert(
                            ev_id,
                            crate::services::fleet_social::fleet_social_persistence::CalendarEventState {
                                event_id: ev_id, owner_id: st_corp, creator_id: char_id,
                                event_date_time: arrival_ft, duration: decay_mins, importance: 1,
                                title: ev_title.clone(), description: ev_desc,
                                _scope: "Automated".into(), responses: HashMap::new(), is_deleted: false,
                            },
                        );
                    }
                    crate::services::fleet_social::persist_social_state(ctx);
                    ctx.notify(
                        NotificationTarget::Corporation(st_corp),
                        "OnNewCalendarEvent",
                        "corpid",
                        vec![
                            PyRep::Integer(ev_id), PyRep::Integer(st_corp), PyRep::Integer(arrival_ft),
                            PyRep::Integer(decay_mins), PyRep::String(ev_title), PyRep::Integer(1),
                            PyRep::Integer(1), PyRep::Bool(true),
                        ],
                    );
                    let chunk_vol_m3 = ((arr.saturating_sub(st)) as f64 / 3600.0) * 30_000.0;
                    let ore_list: Vec<PyRep> = ores
                        .into_iter()
                        .map(|(tid, pct)| {
                            PyRep::keyval([
                                ("typeID", PyRep::Integer(tid as i64)),
                                ("yield", PyRep::Real(pct)),
                                ("volumeM3", PyRep::Real(chunk_vol_m3 * pct)),
                            ])
                        })
                        .collect();

                    let reply = PyRep::keyval([
                        ("structureID", PyRep::Integer(s_id)),
                        ("moonID", PyRep::Integer(m_id)),
                        ("extractionStartTime", PyRep::Integer(st as i64)),
                        ("chunkArrivalTime", PyRep::Integer(arr as i64)),
                        ("naturalDecayTime", PyRep::Integer(dec as i64)),
                        ("chunkVolumeM3", PyRep::Real(chunk_vol_m3)),
                        ("status", PyRep::String("Started".into())),
                        ("eventID", PyRep::Integer(ev_id)),
                        ("readyToFracture", PyRep::Bool(false)),
                        ("moonOres", PyRep::List(ore_list)),
                    ]);
                    ctx.notify(NotificationTarget::Corporation(st_corp), "OnMoonExtractionUpdate", "corpid", vec![reply.clone()]);
                    Some(Ok(reply))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("moonExtractions", "CancelExtraction") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let Some(structure_id) = resolve_moon_structure_id(ctx, session, args, _kwargs) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CancelExtraction requires structureID".into(),
                }));
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(st) = snap.structures.get(&structure_id) else {
                    return Err(BridgeRpcError {
                        code: "StructureNotFound".into(),
                        message: format!("Structure {structure_id} not found"),
                    });
                };
                let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                if st.owner_corp_id != caller_corp
                    || !mining_compression::caller_has_structure_extraction_role(snap, char_id, caller_corp, session.corp_role)
                {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Character lacks permission to cancel extraction".into(),
                    });
                }
                let Some(ext) = snap.moon_extractions.get_mut(&structure_id) else {
                    return Err(BridgeRpcError {
                        code: "NoActiveExtraction".into(),
                        message: format!("No active moon extraction on structure {structure_id}"),
                    });
                };
                if ext.status != "Started" && ext.status != "Ready" {
                    return Err(BridgeRpcError {
                        code: "CannotCancelExtraction".into(),
                        message: format!("Cannot cancel moon extraction in status {}", ext.status),
                    });
                }
                let ev_id = ext.event_id;
                let arrival_ft = (ext.chunk_arrival_unix as i64 + 11_644_473_600) * 10_000_000;
                ext.status = "Cancelled".into();
                if let Some(cev) = snap.calendar_events.get_mut(&ev_id) {
                    cev.is_deleted = true;
                }
                Ok((ev_id, caller_corp, arrival_ft))
            });

            match res {
                Ok((ev_id, caller_corp, arrival_ft)) => {
                    if let Some(cev) = ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner()).calendar_events.get_mut(&ev_id) {
                        cev.is_deleted = true;
                    }
                    crate::services::fleet_social::persist_social_state(ctx);
                    ctx.notify(
                        NotificationTarget::Corporation(caller_corp),
                        "OnRemoveCalendarEvent",
                        "corpid",
                        vec![PyRep::Integer(ev_id), PyRep::Integer(arrival_ft), PyRep::Bool(true)],
                    );
                    ctx.notify(NotificationTarget::Corporation(caller_corp), "OnMoonExtractionUpdate", "corpid", vec![PyRep::None]);
                    Some(Ok(PyRep::Bool(true)))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("moonExtractions", "FractureChunkWithStructure") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let Some(structure_id) = resolve_moon_structure_id(ctx, session, args, _kwargs) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "FractureChunkWithStructure requires structureID".into(),
                }));
            };

            let now = ctx.now_unix();
            // DERIVED: 15% jackpot probability roll via deterministic seeded/CSPRNG `ctx.mechanic_rng`
            let is_jackpot = ctx.mechanic_rng.try_unit_f64().unwrap_or(0.5) < 0.15;

            let commit_res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(st) = snap.structures.get(&structure_id) else {
                    return Err(BridgeRpcError {
                        code: "StructureNotFound".into(),
                        message: format!("Structure {structure_id} not found"),
                    });
                };
                let (sys_id, st_pos, owner_corp_id) = (st.solar_system_id, st.position, st.owner_corp_id);

                let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                if owner_corp_id != caller_corp
                    || !mining_compression::caller_has_structure_extraction_role(
                        snap,
                        char_id,
                        caller_corp,
                        session.corp_role,
                    )
                {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Character lacks corporation permission to fracture chunk".into(),
                    });
                }

                let Some(ext) = snap.moon_extractions.get(&structure_id) else {
                    return Err(BridgeRpcError {
                        code: "NoActiveExtraction".into(),
                        message: format!("No active extraction to fracture on structure {structure_id}"),
                    });
                };
                if ext.status == "Fractured" {
                    return Err(BridgeRpcError {
                        code: "ExtractionAlreadyFractured".into(),
                        message: format!("Moon extraction on structure {structure_id} has already been fractured"),
                    });
                }
                if ext.status != "Started" && ext.status != "Ready" {
                    return Err(BridgeRpcError {
                        code: "InvalidExtractionState".into(),
                        message: format!("Cannot fracture extraction in status {}", ext.status),
                    });
                }
                if now < ext.chunk_arrival_unix {
                    return Err(BridgeRpcError {
                        code: "ChunkNotReadyToFracture".into(),
                        message: format!("Moon chunk has not yet arrived (now={now}, arrival={})", ext.chunk_arrival_unix),
                    });
                }

                let base_ores = if ext.moon_ores.is_empty() {
                    derive_moon_ores_from_sde(&ctx.sde, sys_id, ext.moon_id)
                } else {
                    ext.moon_ores.clone()
                };

                // Moon extraction chunk volume scales linearly at 30,000 m³/hour (`docs/eve-wiki/Moon_mining.md:107-112`)
                let duration_secs = ext.chunk_arrival_unix.saturating_sub(ext.extraction_start_unix).max(518_400);
                let total_chunk_volume_m3 = (duration_secs as f64 / 3600.0) * 30_000.0;

                let (moon_id, start_ts, arr_ts, dec_ts, ev_id) = (
                    ext.moon_id,
                    ext.extraction_start_unix,
                    ext.chunk_arrival_unix,
                    ext.natural_decay_unix,
                    ext.event_id,
                );
                let (spawned_ids, spawned_specs) = build_moon_chunk_asteroids(
                    &ctx.sde, snap, structure_id, sys_id, st_pos, &base_ores, total_chunk_volume_m3, is_jackpot,
                );
                record_fractured_moon_field(snap, structure_id, sys_id, dec_ts, spawned_ids.clone());

                let ext_mut = snap.moon_extractions.get_mut(&structure_id).unwrap();
                ext_mut.status = "Fractured".into();
                ext_mut.auto_fracture = false;
                ext_mut.spawned_asteroid_ids = spawned_ids.clone();

                Ok((sys_id, owner_corp_id, spawned_ids, spawned_specs, total_chunk_volume_m3, moon_id, start_ts, arr_ts, dec_ts, ev_id))
            });

            match commit_res {
                Ok((sys_id, owner_corp_id, ast_ids, spawned_specs, total_chunk_volume_m3, moon_id, start_ts, arr_ts, dec_ts, ev_id)) => {
                    let ore_tids = spawn_moon_asteroid_balls(ctx, sys_id, structure_id, spawned_specs).await;
                    let ast_id_reps: Vec<PyRep> = ast_ids.iter().map(|&id| PyRep::Integer(id)).collect();
                    let reply = PyRep::keyval([
                        ("structureID", PyRep::Integer(structure_id)),
                        ("moonID", PyRep::Integer(moon_id)),
                        ("extractionStartTime", PyRep::Integer(start_ts as i64)),
                        ("chunkArrivalTime", PyRep::Integer(arr_ts as i64)),
                        ("naturalDecayTime", PyRep::Integer(dec_ts as i64)),
                        ("eventID", PyRep::Integer(ev_id)),
                        ("readyToFracture", PyRep::Bool(false)),
                        ("autoFractured", PyRep::Bool(false)),
                        ("fractured", PyRep::Bool(true)),
                        ("status", PyRep::String("Fractured".into())),
                        ("chunkVolumeM3", PyRep::Real(total_chunk_volume_m3)),
                        ("asteroidCount", PyRep::Integer(ast_ids.len() as i64)),
                        ("asteroidIDs", PyRep::List(ast_id_reps)),
                        ("oreTypes", PyRep::List(ore_tids)),
                        ("isJackpot", PyRep::Bool(is_jackpot)),
                        ("jackpotBonus", PyRep::Real(if is_jackpot { 2.0 } else { 1.0 })),
                    ]);
                    ctx.notify(NotificationTarget::Corporation(owner_corp_id), "OnMoonExtractionUpdate", "corpid", vec![reply.clone()]);
                    Some(Ok(reply))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("moonExtractions", "GetExtractionsForCorp") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let caller_corp = ctx.db.with_snapshot(|snap| {
                snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0)
            });
            let corp_id = args.first().and_then(|a| a.as_i64()).unwrap_or(caller_corp);
            if corp_id != caller_corp && !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: format!("Cannot query moon extractions for foreign corporation {corp_id}"),
                }));
            }
            let now = ctx.now_unix();
            let extractions = ctx.db.with_snapshot(|snap| {
                snap.moon_extractions
                    .values()
                    .filter(|ext| snap.structures.get(&ext.structure_id).map(|s| s.owner_corp_id == corp_id).unwrap_or(false))
                    .cloned()
                    .collect::<Vec<_>>()
            });

            let list: Vec<PyRep> = extractions
                .into_iter()
                .map(|e| {
                    let effective_status = if e.status != "Cancelled" && now >= e.natural_decay_unix {
                        "Decayed".to_string()
                    } else if e.status == "Started" && now >= e.chunk_arrival_unix {
                        "Ready".to_string()
                    } else {
                        e.status.clone()
                    };
                    let ready = effective_status == "Ready" || effective_status == "Fractured";
                    PyRep::keyval([
                        ("structureID", PyRep::Integer(e.structure_id)),
                        ("moonID", PyRep::Integer(e.moon_id)),
                        ("extractionStartTime", PyRep::Integer(e.extraction_start_unix as i64)),
                        ("chunkArrivalTime", PyRep::Integer(e.chunk_arrival_unix as i64)),
                        ("naturalDecayTime", PyRep::Integer(e.natural_decay_unix as i64)),
                        ("status", PyRep::String(effective_status)),
                        ("autoFractured", PyRep::Bool(e.auto_fracture)),
                        ("readyToFracture", PyRep::Bool(ready)),
                    ])
                })
                .collect();

            Some(Ok(PyRep::List(list)))
        }

        ("moonExtractions", "GetMoonResourcesForStructure") => {
            let Some(structure_id) = resolve_moon_structure_id(ctx, session, args, _kwargs) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetMoonResourcesForStructure requires structureID".into(),
                }));
            };

            let struct_info = ctx.db.with_snapshot(|snap| {
                let st = snap.structures.get(&structure_id)?;
                let existing_moon = snap.moon_extractions.get(&structure_id).map(|e| e.moon_id);
                Some((st.solar_system_id, st.position, existing_moon))
            });
            let Some((sys_id, pos, existing_moon)) = struct_info else {
                return Some(Err(BridgeRpcError {
                    code: "StructureNotFound".into(),
                    message: format!("Structure {structure_id} not found"),
                }));
            };
            let Some(moon_id) = existing_moon.or_else(|| resolve_structure_moon_id(&ctx.sde, sys_id, pos)) else {
                return Some(Err(BridgeRpcError {
                    code: "NoMoonInSystem".into(),
                    message: format!("No SDE moon celestial found in system {sys_id}"),
                }));
            };

            let resources = derive_moon_ores_from_sde(&ctx.sde, sys_id, moon_id);

            let list: Vec<PyRep> = resources
                .into_iter()
                .map(|(tid, frac)| {
                    let name = ctx
                        .sde
                        .types
                        .get(&tid)
                        .map(|t| t.name.clone())
                        .unwrap_or_else(|| format!("Type {tid}"));
                    PyRep::keyval([
                        ("typeID", PyRep::Integer(tid as i64)),
                        ("oreName", PyRep::String(name)),
                        ("yield", PyRep::Real(frac)),
                        ("moonID", PyRep::Integer(moon_id)),
                    ])
                })
                .collect();

            Some(Ok(PyRep::List(list)))
        }

        ("moonExtractions", "GetExtractionAndEventIDFromStructureID") => {
            let Some(structure_id) = resolve_moon_structure_id(ctx, session, args, _kwargs) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetExtractionAndEventIDFromStructureID requires structureID".into(),
                }));
            };

            let now = ctx.now_unix();
            let (ext, delay_secs) = ctx.db.with_snapshot(|snap| {
                let d = compute_auto_fracture_delay_secs(&ctx.sde, snap, structure_id);
                (snap.moon_extractions.get(&structure_id).cloned(), d)
            });

            if let Some(e) = ext {
                let effective_status = if e.status != "Cancelled" && now >= e.natural_decay_unix {
                    "Decayed".to_string()
                } else if e.status == "Started" && now >= e.chunk_arrival_unix {
                    "Ready".to_string()
                } else {
                    e.status
                };
                let active_count = if effective_status == "Decayed" {
                    0
                } else {
                    e.spawned_asteroid_ids.len() as i64
                };
                let ast_id_reps: Vec<PyRep> = e
                    .spawned_asteroid_ids
                    .iter()
                    .map(|&id| PyRep::Integer(id))
                    .collect();
                let extraction_kv = PyRep::keyval([
                    ("structureID", PyRep::Integer(e.structure_id)),
                    ("moonID", PyRep::Integer(e.moon_id)),
                    ("extractionStartTime", PyRep::Integer(e.extraction_start_unix as i64)),
                    ("chunkArrivalTime", PyRep::Integer(e.chunk_arrival_unix as i64)),
                    ("autoFractureTime", PyRep::Integer(e.chunk_arrival_unix.saturating_add(delay_secs) as i64)),
                    ("naturalDecayTime", PyRep::Integer(e.natural_decay_unix as i64)),
                    ("status", PyRep::String(effective_status)),
                    ("autoFractured", PyRep::Bool(e.auto_fracture)),
                    ("eventID", PyRep::Integer(e.event_id)),
                    ("activeAsteroidCount", PyRep::Integer(active_count)),
                    ("asteroidIDs", PyRep::List(ast_id_reps)),
                ]);
                Some(Ok(PyRep::Tuple(vec![
                    extraction_kv,
                    PyRep::Integer(e.event_id),
                ])))
            } else {
                Some(Ok(PyRep::Tuple(vec![PyRep::None, PyRep::None])))
            }
        }

        // ====================================================================
        // MN-14: Auto Moon Mining Service (autoMoonMining.*)
        // ====================================================================
        ("autoMoonMining", "RequestMiningDetails") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let Some(structure_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| _kwargs.get("structureID").and_then(|v| v.as_i64()))
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "RequestMiningDetails requires structureID".into(),
                }));
            };

            let fuel_per_cycle = metenox_fuel_per_cycle(&ctx.sde);
            let gas_per_cycle: u32 = METENOX_MAGMATIC_GAS_PER_CYCLE;
            let now = ctx.now_unix();

            // Read-only query (`R73 P2-4`, `R-MN14-FALSE`): never mutate `snap.auto_moon_miners` on `RequestMiningDetails`.
            let res = ctx.db.with_snapshot(|snap| {
                let existing = snap.auto_moon_miners.get(&structure_id);
                let efficiency = existing.map(|r| r.efficiency).unwrap_or(0.40);
                let cycle_duration_seconds = existing.map(|r| r.cycle_duration_seconds).unwrap_or(3600);
                let last_cycle_unix = existing.map(|r| r.last_cycle_unix).unwrap_or(now);

                let info = inspect_metenox_drill(&ctx.sde, snap, structure_id, char_id, efficiency)?;
                let (remaining_cap, bay_units) = metenox_output_bay_remaining_capacity(&ctx.sde, snap, structure_id);
                let per_cycle_vol: f64 = info
                    .hourly_materials
                    .iter()
                    .map(|&(tid, qty)| {
                        let uvol = ctx.sde.types.get(&tid).map(|t| t.volume).unwrap_or(0.1);
                        (qty as f64) * uvol
                    })
                    .sum();
                let status = if info.total_struct_fuel < fuel_per_cycle {
                    "OutOfFuel".into()
                } else if info.total_struct_gas < gas_per_cycle {
                    "OutOfMagmaticGas".into()
                } else if remaining_cap < per_cycle_vol {
                    "OutputBayFull".into()
                } else {
                    "Active".into()
                };

                let view = AutoMoonMinerRecord {
                    structure_id,
                    fuel_units_remaining: info.total_struct_fuel,
                    magmatic_gas_remaining: info.total_struct_gas,
                    efficiency,
                    cycle_duration_seconds,
                    last_cycle_unix,
                    ore_type_id: info.primary_mat_tid,
                    hourly_yield: info.hourly_yield,
                    status,
                };

                Ok((view, info.moon_id, info.hourly_materials, remaining_cap, bay_units))
            });

            match res {
                Ok((rec, moon_id, mats, remaining_cap, bay_units)) => {
                    let next_harvest_unix = rec.last_cycle_unix.saturating_add(rec.cycle_duration_seconds);
                    let next_harvest_filetime = (next_harvest_unix as i64 + 11_644_473_600) * 10_000_000;
                    let output_materials: Vec<PyRep> = mats
                        .iter()
                        .map(|&(tid, qty)| {
                            let uvol = ctx.sde.types.get(&tid).map(|t| t.volume).unwrap_or(0.1);
                            PyRep::Tuple(vec![
                                PyRep::Integer(tid as i64),
                                PyRep::Integer(qty as i64),
                                PyRep::Real((qty as f64) * uvol),
                            ])
                        })
                        .collect();
                    let mat_list: Vec<PyRep> = mats
                        .into_iter()
                        .map(|(tid, qty)| {
                            PyRep::keyval([
                                ("typeID", PyRep::Integer(tid as i64)),
                                ("quantity", PyRep::Integer(qty as i64)),
                            ])
                        })
                        .collect();
                    Some(Ok(PyRep::keyval([
                        ("availableFuel", PyRep::Integer(rec.fuel_units_remaining as i64)),
                        ("moonID", PyRep::Integer(moon_id)),
                        ("nextHarvestTime", PyRep::Integer(next_harvest_filetime)),
                        ("outputMaterials", PyRep::List(output_materials)),
                        ("remainingOutputBayCapacity", PyRep::Real(remaining_cap)),
                        ("structureID", PyRep::Integer(rec.structure_id)),
                        ("efficiency", PyRep::Real(rec.efficiency)),
                        ("cycleTimeSeconds", PyRep::Integer(rec.cycle_duration_seconds as i64)),
                        ("fuelConsumptionPerCycle", PyRep::Integer(fuel_per_cycle as i64)),
                        ("magmaticGasPerCycle", PyRep::Integer(gas_per_cycle as i64)),
                        ("fuelRemaining", PyRep::Integer(rec.fuel_units_remaining as i64)),
                        ("magmaticGasRemaining", PyRep::Integer(rec.magmatic_gas_remaining as i64)),
                        ("status", PyRep::String(rec.status)),
                        ("hourlyYield", PyRep::Integer(rec.hourly_yield as i64)),
                        ("oreTypeID", PyRep::Integer(rec.ore_type_id as i64)),
                        ("primaryMaterialTypeID", PyRep::Integer(rec.ore_type_id as i64)),
                        ("hourlyMoonMaterials", PyRep::List(mat_list)),
                        ("harvestedUnitsInBay", PyRep::Integer(bay_units)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("autoMoonMining", "RequestMiningCycleOutput") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let Some(structure_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| _kwargs.get("structureID").and_then(|v| v.as_i64()))
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "RequestMiningCycleOutput requires structureID".into(),
                }));
            };

            // Read-only query (`client_expectations.json:50680-50681`, `R-MN14-FALSE`):
            // returns per-cycle materials list `[(type_id, quantity, volume), ...]` for `OutputMaterialsSection(materials_per_cycle=)`.
            let res = ctx.db.with_snapshot(|snap| {
                let efficiency = snap
                    .auto_moon_miners
                    .get(&structure_id)
                    .map(|r| r.efficiency)
                    .unwrap_or(0.40);
                let info = inspect_metenox_drill(&ctx.sde, snap, structure_id, char_id, efficiency)?;
                let tuples: Vec<PyRep> = info
                    .hourly_materials
                    .into_iter()
                    .map(|(tid, qty)| {
                        let uvol = ctx.sde.types.get(&tid).map(|t| t.volume).unwrap_or(0.1);
                        PyRep::Tuple(vec![
                            PyRep::Integer(tid as i64),
                            PyRep::Integer(qty as i64),
                            PyRep::Real((qty as f64) * uvol),
                        ])
                    })
                    .collect();
                Ok(PyRep::List(tuples))
            });

            Some(res)
        }

        _ => mining_compression::handle_compression(ctx, session, service, method, args, _kwargs)
            .await,
    }
}

fn metenox_fuel_per_cycle(sde: &SdeRegistry) -> u32 {
    sde.types.get(&82941).and_then(|t| t.default_attributes.get(&2109).copied()).unwrap_or(5.0).round().max(1.0) as u32
}

fn metenox_output_bay_remaining_capacity(sde: &SdeRegistry, snap: &eve_db::WorldStateSnapshot, structure_id: i64) -> (f64, i64) {
    // DERIVED: SDE type 81826 (Metenox Moon Drill) capacity attribute = 480,000 m³.
    let default_cap = sde.types.get(&81826).map(|t| t.capacity).filter(|&c| c > 0.0).unwrap_or(480_000.0);
    let total_cap = snap.structures.get(&structure_id).and_then(|st| sde.types.get(&st.type_id)).map(|t| t.capacity).filter(|&c| c > 0.0).unwrap_or(default_cap);
    let mut used_vol = 0.0_f64;
    let mut bay_units = 0_i64;
    for it in snap.items.values() {
        if it.location_id == structure_id && matches!(it.flag, InventoryFlag::SpecializedMineralHold | InventoryFlag::SpecializedMaterialBay) && it.quantity > 0 {
            let uvol = sde.types.get(&it.type_id).map(|t| t.volume).unwrap_or(0.1);
            used_vol += (it.quantity as f64) * uvol;
            if sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 427) {
                bay_units += it.quantity as i64;
            }
        }
    }
    ((total_cap - used_vol).max(0.0), bay_units)
}

/// Server-driven hourly Metenox Moon Drill production tick (`plan:MN-14`, `R-MN14-FALSE`, `R-MN14-PROD-TICK`).
pub fn tick_all_metenox_moon_drills(ctx: &ServerContext, prev_unix: u64, now_unix: u64) {
    let fuel_per_cycle = metenox_fuel_per_cycle(&ctx.sde);
    let gas_per_cycle: u32 = METENOX_MAGMATIC_GAS_PER_CYCLE;

    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert("metenox_last_tick_unix".into(), now_unix.to_string());
        // SDE group 4744 (Upwell Moon Drill, type 81826 Metenox Moon Drill); extraction pauses while anchoring or reinforced.
        let metenox_ids: Vec<(i64, u32, i64, [f64; 3])> = snap.structures.values()
            .filter(|st| !matches!(st.state.as_str(), "Destroyed" | "Anchoring" | "Reinforced") && ctx.sde.types.get(&st.type_id).is_some_and(|t| t.group_id == 4744))
            .map(|st| (st.structure_id, st.solar_system_id, st.owner_corp_id, st.position))
            .collect();
        if metenox_ids.is_empty() { return; }

        for (structure_id, st_sys_id, st_owner_corp, st_pos) in metenox_ids {
            let sec = ctx.sde.solar_systems.get(&st_sys_id).map(|s| s.security_status).unwrap_or(0.0);
            if sec >= 0.45 && st_sys_id < 31_000_000 { continue; }
            let Some(moon_id) = resolve_structure_moon_id(&ctx.sde, st_sys_id, st_pos) else { continue; };
            let efficiency = snap.auto_moon_miners.get(&structure_id).map(|r| r.efficiency).unwrap_or(0.40);
            let cycle_secs = snap.auto_moon_miners.get(&structure_id).map(|r| r.cycle_duration_seconds.max(1) as u64).unwrap_or(3600);
            let hourly_materials = derive_metenox_moon_materials(&ctx.sde, st_sys_id, moon_id, efficiency, 1);
            let Some(&(primary_mat_tid, _)) = hourly_materials.first() else { continue; };
            let hourly_yield: u64 = hourly_materials.iter().map(|&(_, q)| q).sum();
            let per_cycle_vol: f64 = hourly_materials.iter().map(|&(tid, qty)| (qty as f64) * ctx.sde.types.get(&tid).map(|t| t.volume).unwrap_or(0.1)).sum();

            let count_reagents = |snap_ref: &eve_db::WorldStateSnapshot| -> (u32, u32) {
                let st_fuel = snap_ref.structures.get(&structure_id).map(|s| s.fuel_blocks).unwrap_or(0);
                let mut item_fuel = 0u32;
                let mut gas = 0u32;
                for it in snap_ref.items.values() {
                    if it.location_id == structure_id && (st_owner_corp == 0 || it.owner_id == st_owner_corp) && it.quantity > 0 && it.flag != InventoryFlag::Locked && !snap_ref.is_item_locked_in_contract_or_job(it.item_id) {
                        if ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 1136) {
                            item_fuel = item_fuel.saturating_add(it.quantity as u32);
                        } else if it.type_id == 81143 {
                            gas = gas.saturating_add(it.quantity as u32);
                        }
                    }
                }
                (st_fuel.saturating_add(item_fuel), gas)
            };

            let (init_fuel, init_gas) = count_reagents(snap);
            let last_cycle_unix = snap.auto_moon_miners.entry(structure_id).or_insert_with(|| AutoMoonMinerRecord {
                structure_id, fuel_units_remaining: init_fuel, magmatic_gas_remaining: init_gas, efficiency,
                cycle_duration_seconds: cycle_secs, last_cycle_unix: prev_unix, ore_type_id: primary_mat_tid,
                hourly_yield, status: "Active".into(),
            }).last_cycle_unix;

            let elapsed_cycles = (now_unix.saturating_sub(last_cycle_unix) / cycle_secs).min(1_000) as u32;
            if elapsed_cycles == 0 { continue; }

            let (rem_cap_before, _) = metenox_output_bay_remaining_capacity(&ctx.sde, snap, structure_id);
            let max_cap_cycles = if per_cycle_vol > 0.0 { (rem_cap_before / per_cycle_vol).floor() as u32 } else { elapsed_cycles };
            let runnable_cycles = elapsed_cycles.min(init_fuel / fuel_per_cycle).min(init_gas / gas_per_cycle).min(max_cap_cycles);

            if runnable_cycles > 0 {
                let mut rem_fuel_to_deduct = runnable_cycles * fuel_per_cycle;
                let mut rem_gas_to_deduct = runnable_cycles * gas_per_cycle;
                if let Some(st_mut) = snap.structures.get_mut(&structure_id) {
                    let from_bay = st_mut.fuel_blocks.min(rem_fuel_to_deduct);
                    st_mut.fuel_blocks -= from_bay;
                    rem_fuel_to_deduct -= from_bay;
                }
                for (rem_ref, is_fuel) in [(&mut rem_fuel_to_deduct, true), (&mut rem_gas_to_deduct, false)] {
                    if *rem_ref > 0 {
                        let mut item_ids: Vec<i64> = snap.items.values()
                            .filter(|it| it.location_id == structure_id && (st_owner_corp == 0 || it.owner_id == st_owner_corp) && it.quantity > 0 && it.flag != InventoryFlag::Locked && !snap.is_item_locked_in_contract_or_job(it.item_id) && if is_fuel { ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 1136) } else { it.type_id == 81143 })
                            .map(|it| it.item_id).collect();
                        item_ids.sort_unstable();
                        for iid in item_ids {
                            if *rem_ref == 0 { break; }
                            if let Some(it) = snap.items.get_mut(&iid) {
                                let take = (it.quantity.max(0) as u32).min(*rem_ref);
                                it.quantity -= take as i32;
                                *rem_ref -= take;
                                if it.quantity <= 0 { snap.items.remove(&iid); }
                            }
                        }
                    }
                }

                for &(mat_tid, per_cycle_qty) in &hourly_materials {
                    let mat_qty = per_cycle_qty.saturating_mul(runnable_cycles as u64);
                    let add_qty = i32::try_from(mat_qty).unwrap_or(i32::MAX);
                    if let Some(existing_item) = snap.items.values_mut().find(|it| it.location_id == structure_id && it.owner_id == st_owner_corp && it.flag == InventoryFlag::SpecializedMineralHold && it.type_id == mat_tid && !it.singleton) {
                        existing_item.quantity = existing_item.quantity.saturating_add(add_qty);
                    } else {
                        snap.next_entity_id += 1;
                        let item_id = snap.next_entity_id;
                        snap.items.insert(item_id, InventoryItemRecord {
                            item_id, type_id: mat_tid, owner_id: st_owner_corp, location_id: structure_id,
                            flag: InventoryFlag::SpecializedMineralHold, quantity: add_qty,
                            singleton: false, version: 1, ..Default::default()
                        });
                    }
                }
            }

            let (after_fuel, after_gas) = count_reagents(snap);
            let (rem_cap_after, _) = metenox_output_bay_remaining_capacity(&ctx.sde, snap, structure_id);
            if let Some(am) = snap.auto_moon_miners.get_mut(&structure_id) {
                am.fuel_units_remaining = after_fuel;
                am.magmatic_gas_remaining = after_gas;
                am.ore_type_id = primary_mat_tid;
                am.hourly_yield = hourly_yield;
                am.last_cycle_unix = if runnable_cycles < elapsed_cycles { now_unix } else { last_cycle_unix.saturating_add((runnable_cycles as u64) * cycle_secs) };
                am.status = if after_fuel < fuel_per_cycle { "OutOfFuel".into() } else if after_gas < gas_per_cycle { "OutOfMagmaticGas".into() } else if rem_cap_after < per_cycle_vol { "OutputBayFull".into() } else { "Active".into() };
            }
        }
    });
}

