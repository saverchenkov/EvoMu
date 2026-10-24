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

use crate::router::ServerContext;
use eve_db::{
    AutoMoonMinerRecord, InventoryFlag, InventoryItemRecord, MoonExtractionRecord,
    ObserverMiningRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use glam::DVec3;
use std::collections::HashMap;

/// Canonical R4 moon ores used as fallback only if SDE moon ore groups are unavailable:
#[allow(dead_code)]
const MOON_ORE_ZEOLITES: TypeId = 45490;
#[allow(dead_code)]
const MOON_ORE_SYLVITE: TypeId = 45491;
#[allow(dead_code)]
const MOON_ORE_BITUMENS: TypeId = 45492;
#[allow(dead_code)]
const MOON_ORE_COESITE: TypeId = 45493;
#[allow(dead_code)]
const MOON_ORE_COBALTITE: TypeId = 45494;

/// Resolves the nearest SDE moon celestial (`group_id == 8`) in `solar_system_id` to `pos`.
fn resolve_structure_moon_id(
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
        t.published
            && !t.name.contains("Compressed")
            && !t.name.contains("Brimful")
            && !t.name.contains("Bountiful")
            && !t.name.contains("Shining")
            && !t.name.contains("Glistening")
            && !t.name.contains("Glowing")
            && !t.name.contains("Twinkling")
            && !t.name.contains("Shimmering")
            && !t.name.contains("Resplendent")
            && !t.name.contains("Sparkling")
            && !t.name.contains("Dazzling")
            && !t.name.contains("Lavish")
    };
    let mut base_moon_ores: Vec<TypeId> = sde
        .types
        .values()
        .filter(|t| {
            let group_ok = if restrict_to_r4 {
                t.group_id == 1884
            } else {
                matches!(t.group_id, 1884 | 1920..=1923)
            };
            group_ok && is_base_moon_ore(t)
        })
        .map(|t| t.type_id)
        .collect();
    base_moon_ores.sort_unstable();
    if base_moon_ores.len() < 3 {
        return vec![
            (MOON_ORE_BITUMENS, 0.40),
            (MOON_ORE_ZEOLITES, 0.35),
            (MOON_ORE_COBALTITE, 0.25),
        ];
    }
    let h = (moon_id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut idx0 = (h as usize) % base_moon_ores.len();
    if !restrict_to_r4 {
        let higher_ores: Vec<usize> = base_moon_ores
            .iter()
            .enumerate()
            .filter(|(_, &tid)| {
                sde.types
                    .get(&tid)
                    .is_some_and(|t| matches!(t.group_id, 1920..=1923))
            })
            .map(|(i, _)| i)
            .collect();
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

async fn maybe_auto_fracture_moon_extraction(ctx: &ServerContext, structure_id: i64) {
    let now = ctx.now_unix();
    let auto_res = ctx.db.try_with_mut_snapshot(|snap| -> Result<Option<(u32, Vec<(i64, TypeId, u64, f64, DVec3)>)>, BridgeRpcError> {
        let Some(st) = snap.structures.get(&structure_id) else {
            return Ok(None);
        };
        let sys_id = st.solar_system_id;
        let st_pos = st.position;
        let delay_secs = compute_auto_fracture_delay_secs(&ctx.sde, snap, structure_id);
        let Some(ext) = snap.moon_extractions.get(&structure_id) else {
            return Ok(None);
        };
        if (ext.status != "Started" && ext.status != "Ready")
            || now < ext.chunk_arrival_unix.saturating_add(delay_secs)
            || now >= ext.natural_decay_unix
        {
            return Ok(None);
        }
        let base_ores = if ext.moon_ores.is_empty() {
            derive_moon_ores_from_sde(&ctx.sde, sys_id, ext.moon_id)
        } else {
            ext.moon_ores.clone()
        };
        let duration_secs = ext
            .chunk_arrival_unix
            .saturating_sub(ext.extraction_start_unix)
            .max(518_400);
        let total_chunk_volume_m3 = (duration_secs as f64 / 3600.0) * 30_000.0;
        let (spawned_ids, spawned_specs) = build_moon_chunk_asteroids(
            &ctx.sde,
            snap,
            structure_id,
            sys_id,
            st_pos,
            &base_ores,
            total_chunk_volume_m3,
            false,
        );
        if let Some(ext_mut) = snap.moon_extractions.get_mut(&structure_id) {
            ext_mut.status = "Fractured".into();
            ext_mut.auto_fracture = true;
            ext_mut.spawned_asteroid_ids = spawned_ids;
        }
        Ok(Some((sys_id, spawned_specs)))
    });
    if let Ok(Some((sys_id, spawned_specs))) = auto_res {
        let handle = ctx.systems.get_or_spawn(sys_id);
        for (ast_id, ore_tid, units, uvol, pos) in spawned_specs {
            let ball = Ball {
                id: ast_id,
                type_id: ore_tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000.0,
                radius: 60.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            };
            let ast_state = crate::system_actor::AsteroidState {
                asteroid_id: ast_id,
                belt_id: structure_id,
                type_id: ore_tid,
                initial_units: units,
                remaining_units: units,
                unit_volume: uvol,
                depleted: false,
                respawn_at_tick: None,
            };
            let _ = handle.spawn_asteroid(ast_state, ball).await;
        }
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

            let Some(structure_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "StartNewExtraction requires structureID".into(),
                }));
            };

            // Server-authoritative extraction timings (`docs/eve-wiki/Moon_mining.md:107-112`):
            // duration must be in 6d..=56d (`518_400..=4_838_400` seconds, default 6d = 518,400s),
            // natural decay window fixed at 48h (172,800s) after chunk arrival.
            let now = ctx.now_unix();
            let explicit_dur = args.get(1).and_then(|a| a.as_i64());
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
                if st.owner_corp_id != caller_corp {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: format!(
                            "Character {char_id} (corp {caller_corp}) lacks permissions on structure {structure_id} (corp {})",
                            st.owner_corp_id
                        ),
                    });
                }

                if let Some(existing) = snap.moon_extractions.get(&structure_id) {
                    let is_active = (existing.status == "Started" || existing.status == "Ready")
                        && now < existing.natural_decay_unix;
                    if is_active {
                        return Err(BridgeRpcError {
                            code: "ExtractionAlreadyActive".into(),
                            message: format!(
                                "Structure {structure_id} already has an active moon extraction in state {}",
                                existing.status
                            ),
                        });
                    }
                }

                let Some(moon_id) = resolve_structure_moon_id(&ctx.sde, st.solar_system_id, st.position) else {
                    return Err(BridgeRpcError {
                        code: "NoMoonInSystem".into(),
                        message: format!("No SDE moon celestial found in system {}", st.solar_system_id),
                    });
                };

                let moon_ores = derive_moon_ores_from_sde(&ctx.sde, st.solar_system_id, moon_id);

                snap.next_entity_id += 1;
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
                snap.calendar_events.insert(
                    event_id,
                    eve_db::CalendarEventRecord {
                        event_id,
                        owner_id: st_corp,
                        creator_id: char_id,
                        event_date_time: arrival_ft,
                        duration: decay_mins,
                        importance: 1,
                        title: format!("Moon Extraction Ready: {moon_name} ({st_name})"),
                        description: format!(
                            "Automated moon extraction chunk arrival at {st_name} orbiting {moon_name}"
                        ),
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

                Ok((structure_id, moon_id, start_time, arrival_time, decay_time, event_id, moon_ores))
            });

            match res {
                Ok((s_id, m_id, st, arr, dec, ev_id, ores)) => {
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

                    Some(Ok(PyRep::keyval([
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
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("moonExtractions", "CancelExtraction") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let Some(structure_id) = args.first().and_then(|a| a.as_i64()) else {
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
                let caller_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0);
                if st.owner_corp_id != caller_corp {
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
                let ev_id = ext.event_id;
                ext.status = "Cancelled".into();
                ext.spawned_asteroid_ids.clear();
                if let Some(cev) = snap.calendar_events.get_mut(&ev_id) {
                    cev.is_deleted = true;
                }
                Ok(ev_id)
            });

            match res {
                Ok(ev_id) => {
                    if let Some(cev) = ctx.social_extras.lock().unwrap().calendar_events.get_mut(&ev_id) {
                        cev.is_deleted = true;
                    }
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

            let Some(structure_id) = args.first().and_then(|a| a.as_i64()) else {
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
                let sys_id = st.solar_system_id;
                let st_pos = st.position;
                let owner_corp_id = st.owner_corp_id;

                let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                if owner_corp_id != caller_corp {
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

                let (spawned_ids, spawned_specs) = build_moon_chunk_asteroids(
                    &ctx.sde, snap, structure_id, sys_id, st_pos, &base_ores, total_chunk_volume_m3, is_jackpot,
                );

                let ext_mut = snap.moon_extractions.get_mut(&structure_id).unwrap();
                ext_mut.status = "Fractured".into();
                ext_mut.auto_fracture = false;
                ext_mut.spawned_asteroid_ids = spawned_ids.clone();

                Ok((sys_id, spawned_ids, spawned_specs, total_chunk_volume_m3))
            });

            match commit_res {
                Ok((sys_id, ast_ids, spawned_specs, total_chunk_volume_m3)) => {
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
                    let ast_id_reps: Vec<PyRep> = ast_ids.iter().map(|&id| PyRep::Integer(id)).collect();
                    Some(Ok(PyRep::keyval([
                        ("structureID", PyRep::Integer(structure_id)),
                        ("fractured", PyRep::Bool(true)),
                        ("status", PyRep::String("Fractured".into())),
                        ("chunkVolumeM3", PyRep::Real(total_chunk_volume_m3)),
                        ("asteroidCount", PyRep::Integer(ast_ids.len() as i64)),
                        ("asteroidIDs", PyRep::List(ast_id_reps)),
                        ("oreTypes", PyRep::List(ore_tids)),
                        ("isJackpot", PyRep::Bool(is_jackpot)),
                        ("jackpotBonus", PyRep::Real(if is_jackpot { 2.0 } else { 1.0 })),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("moonExtractions", "GetExtractionsForCorp") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = args.first().and_then(|a| a.as_i64()).unwrap_or_else(|| {
                ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0))
            });
            let struct_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
                snap.moon_extractions
                    .keys()
                    .copied()
                    .filter(|sid| snap.structures.get(sid).map(|s| s.owner_corp_id == corp_id).unwrap_or(false))
                    .collect()
            });
            for sid in struct_ids {
                maybe_auto_fracture_moon_extraction(ctx, sid).await;
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
            let Some(structure_id) = args.first().and_then(|a| a.as_i64()) else {
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
            let Some(structure_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetExtractionAndEventIDFromStructureID requires structureID".into(),
                }));
            };

            maybe_auto_fracture_moon_extraction(ctx, structure_id).await;

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
                Some(Ok(PyRep::keyval([
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
                ])))
            } else {
                Some(Ok(PyRep::None))
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

            let fuel_per_cycle = ctx
                .sde
                .types
                .get(&82941)
                .and_then(|t| t.default_attributes.get(&2109).copied())
                .unwrap_or(5.0)
                .round()
                .max(1.0) as u32;
            // DERIVED: Metenox Moon Drill consumes Magmatic Gas (`type_id == 81143`) per hourly cycle (`docs/spec/03_economy_industry_social.md:64`).
            let gas_per_cycle: u32 = METENOX_MAGMATIC_GAS_PER_CYCLE;

            let now = ctx.now_unix();
            // Read-only query (`R73 P2-4`): never mutate `snap.auto_moon_miners` on `RequestMiningDetails`.
            let res = ctx.db.with_snapshot(|snap| {
                let existing = snap.auto_moon_miners.get(&structure_id);
                let efficiency = existing.map(|r| r.efficiency).unwrap_or(0.40);
                let cycle_duration_seconds = existing.map(|r| r.cycle_duration_seconds).unwrap_or(3600);
                let last_cycle_unix = existing.map(|r| r.last_cycle_unix).unwrap_or(now);

                let info = inspect_metenox_drill(&ctx.sde, snap, structure_id, char_id, efficiency)?;
                let status = if info.total_struct_fuel >= fuel_per_cycle
                    && info.total_struct_gas >= gas_per_cycle
                {
                    "Active".into()
                } else if info.total_struct_fuel < fuel_per_cycle {
                    "OutOfFuel".into()
                } else {
                    "OutOfMagmaticGas".into()
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

                Ok((view, info.hourly_materials))
            });

            match res {
                Ok((rec, mats)) => {
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

            let requested_cycles: Option<u32> = match args.get(1) {
                None | Some(PyRep::None) => None,
                Some(v) => {
                    let Some(raw) = v.as_i64() else {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message: "cycles must be an integer".into(),
                        }));
                    };
                    if !(1..=1_000).contains(&raw) {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message: format!("cycles {raw} out of valid range 1..=1000"),
                        }));
                    }
                    Some(raw as u32)
                }
            };

            let fuel_per_cycle = ctx
                .sde
                .types
                .get(&82941)
                .and_then(|t| t.default_attributes.get(&2109).copied())
                .unwrap_or(5.0)
                .round()
                .max(1.0) as u32;
            // DERIVED: Metenox Moon Drill consumes Magmatic Gas (`type_id == 81143`) per hourly cycle (`docs/spec/03_economy_industry_social.md:64`).
            let gas_per_cycle: u32 = METENOX_MAGMATIC_GAS_PER_CYCLE;

            let now = ctx.now_unix();
            let default_start_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(now)
                .min(now);
            let date = eve_db::unix_secs_to_date_string(now);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let info = inspect_metenox_drill(&ctx.sde, snap, structure_id, char_id, 0.40)?;
                let st_owner_corp = info.st_owner_corp;
                let caller_corp = info.caller_corp;
                let st_sys_id = info.st_sys_id;
                let moon_id = info.moon_id;
                let total_struct_fuel = info.total_struct_fuel;
                let total_struct_gas = info.total_struct_gas;
                let primary_mat_tid = info.primary_mat_tid;
                let hourly_yield = info.hourly_yield;
                let matches_reagent_owner = |item_owner_id: i64| -> bool {
                    if st_owner_corp != 0 {
                        item_owner_id == st_owner_corp
                    } else {
                        item_owner_id == char_id || item_owner_id == caller_corp
                    }
                };

                let (cycles, efficiency, fuel_req, gas_req, fuel_rem) = {
                    let auto_miner = snap.auto_moon_miners.entry(structure_id).or_insert_with(|| {
                        AutoMoonMinerRecord {
                            structure_id,
                            fuel_units_remaining: total_struct_fuel,
                            magmatic_gas_remaining: total_struct_gas,
                            efficiency: 0.40,
                            cycle_duration_seconds: 3600,
                            last_cycle_unix: default_start_unix,
                            ore_type_id: primary_mat_tid,
                            hourly_yield,
                            status: "Active".into(),
                        }
                    });
                    // Authoritative sync from structure inventory (no `.max()` with stale record):
                    auto_miner.fuel_units_remaining = total_struct_fuel;
                    auto_miner.magmatic_gas_remaining = total_struct_gas;
                    auto_miner.hourly_yield = hourly_yield;
                    auto_miner.ore_type_id = primary_mat_tid;

                    let cycle_secs = (auto_miner.cycle_duration_seconds.max(1)) as u64;
                    let elapsed_secs = now.saturating_sub(auto_miner.last_cycle_unix);
                    let elapsed_cycles = (elapsed_secs / cycle_secs).min(1_000) as u32;
                    if elapsed_cycles == 0 {
                        return Err(BridgeRpcError {
                            code: "CycleNotComplete".into(),
                            message: format!(
                                "Metenox Moon Drill cycle has not completed yet ({elapsed_secs}s / {cycle_secs}s elapsed)"
                            ),
                        });
                    }
                    let cycles = match requested_cycles {
                        Some(req) if req > elapsed_cycles => {
                            return Err(BridgeRpcError {
                                code: "CycleNotComplete".into(),
                                message: format!(
                                    "Requested {req} cycles but only {elapsed_cycles} cycles have elapsed since last harvest"
                                ),
                            });
                        }
                        Some(req) => req,
                        None => elapsed_cycles,
                    };

                    let fuel_req = cycles.checked_mul(fuel_per_cycle).ok_or_else(|| BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "Fuel requirement overflow".into(),
                    })?;
                    let gas_req = cycles.checked_mul(gas_per_cycle).ok_or_else(|| BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "Magmatic gas requirement overflow".into(),
                    })?;

                    if auto_miner.fuel_units_remaining < fuel_req {
                        auto_miner.status = "OutOfFuel".into();
                        return Err(BridgeRpcError {
                            code: "OutOfFuel".into(),
                            message: format!(
                                "Auto moon drill has {} fuel blocks in structure inventory, but {} required for {cycles} cycles",
                                auto_miner.fuel_units_remaining, fuel_req
                            ),
                        });
                    }
                    if auto_miner.magmatic_gas_remaining < gas_req {
                        auto_miner.status = "OutOfMagmaticGas".into();
                        return Err(BridgeRpcError {
                            code: "OutOfMagmaticGas".into(),
                            message: format!(
                                "Auto moon drill has {} Magmatic Gas (type 81143) in structure inventory, but {} required for {cycles} cycles",
                                auto_miner.magmatic_gas_remaining, gas_req
                            ),
                        });
                    }

                    auto_miner.fuel_units_remaining -= fuel_req;
                    auto_miner.magmatic_gas_remaining -= gas_req;
                    auto_miner.last_cycle_unix += (cycles as u64) * cycle_secs;
                    auto_miner.status = "Active".into();

                    (
                        cycles,
                        auto_miner.efficiency,
                        fuel_req,
                        gas_req,
                        auto_miner.fuel_units_remaining,
                    )
                };

                // Deduct consumed fuel blocks and Magmatic Gas from structure inventory
                let mut rem_fuel_to_deduct = fuel_req;
                let mut rem_gas_to_deduct = gas_req;
                if let Some(st_mut) = snap.structures.get_mut(&structure_id) {
                    let from_bay = st_mut.fuel_blocks.min(rem_fuel_to_deduct);
                    st_mut.fuel_blocks -= from_bay;
                    rem_fuel_to_deduct -= from_bay;
                }
                for (rem_ref, is_fuel, err_code, label) in [
                    (&mut rem_fuel_to_deduct, true, "OutOfFuel", "fuel blocks"),
                    (&mut rem_gas_to_deduct, false, "OutOfMagmaticGas", "Magmatic Gas (type 81143)"),
                ] {
                    if *rem_ref > 0 {
                        let mut item_ids: Vec<i64> = snap
                            .items
                            .values()
                            .filter(|it| {
                                it.location_id == structure_id
                                    && matches_reagent_owner(it.owner_id)
                                    && it.quantity > 0
                                    && it.flag != InventoryFlag::Locked
                                    && !snap.is_item_locked_in_contract_or_job(it.item_id)
                                    && if is_fuel {
                                        ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 1136)
                                    } else {
                                        it.type_id == 81143
                                    }
                            })
                            .map(|it| it.item_id)
                            .collect();
                        item_ids.sort_unstable();
                        for iid in item_ids {
                            if *rem_ref == 0 {
                                break;
                            }
                            if let Some(it) = snap.items.get_mut(&iid) {
                                let take = (it.quantity.max(0) as u32).min(*rem_ref);
                                it.quantity -= take as i32;
                                *rem_ref -= take;
                                if it.quantity <= 0 {
                                    snap.items.remove(&iid);
                                }
                            }
                        }
                        if *rem_ref > 0 {
                            return Err(BridgeRpcError {
                                code: err_code.into(),
                                message: format!("Insufficient {label} in structure {structure_id} ({rem_ref} short)"),
                            });
                        }
                    }
                }

                // Compute harvested Moon Materials (`group_id == 427`, e.g. `16633..=16653`) for `cycles` hours
                let cycle_materials =
                    derive_metenox_moon_materials(&ctx.sde, st_sys_id, moon_id, efficiency, cycles);
                if cycle_materials.is_empty() {
                    return Err(BridgeRpcError {
                        code: "NoMoonMaterials".into(),
                        message: format!("Moon {moon_id} in system {st_sys_id} yields no group-427 moon materials"),
                    });
                }
                let units_produced: u64 = cycle_materials.iter().map(|&(_, q)| q).sum();

                // Deliver harvested Moon Materials to structure output bay / hangar and record in observer ledger
                for &(mat_tid, mat_qty) in &cycle_materials {
                    snap.next_entity_id += 1;
                    let item_id = snap.next_entity_id;
                    snap.items.insert(
                        item_id,
                        InventoryItemRecord {
                            item_id,
                            type_id: mat_tid,
                            owner_id: st_owner_corp,
                            location_id: structure_id,
                            flag: InventoryFlag::Hangar,
                            quantity: mat_qty as i32,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        },
                    );

                    snap.observer_mining_ledger
                        .entry(structure_id)
                        .or_default()
                        .push(ObserverMiningRecord {
                            observer_id: structure_id,
                            character_id: char_id,
                            recorded_corporation_id: caller_corp,
                            type_id: mat_tid,
                            quantity: mat_qty as i64,
                            last_updated_date: date.clone(),
                            timestamp_unix: now,
                        });
                }

                Ok((
                    structure_id,
                    cycles,
                    units_produced,
                    primary_mat_tid,
                    cycle_materials,
                    fuel_req,
                    gas_req,
                    fuel_rem,
                ))
            });

            match res {
                Ok((s_id, cyc, produced, primary_tid, mats, f_used, g_used, f_rem)) => {
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
                        ("structureID", PyRep::Integer(s_id)),
                        ("cyclesProcessed", PyRep::Integer(cyc as i64)),
                        ("unitsProduced", PyRep::Integer(produced as i64)),
                        ("oreTypeID", PyRep::Integer(primary_tid as i64)),
                        ("primaryMaterialTypeID", PyRep::Integer(primary_tid as i64)),
                        ("materialsProduced", PyRep::List(mat_list)),
                        ("fuelConsumed", PyRep::Integer(f_used as i64)),
                        ("magmaticGasConsumed", PyRep::Integer(g_used as i64)),
                        ("fuelRemaining", PyRep::Integer(f_rem as i64)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        _ => mining_compression::handle_compression(ctx, session, service, method, args, _kwargs)
            .await,
    }
}
