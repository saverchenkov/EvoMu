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
use eve_proto::dbrow::{DBRowDescriptor, DbColumnType};
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
    let mut base_moon_ores: Vec<TypeId> = sde
        .types
        .values()
        .filter(|t| {
            let group_ok = if restrict_to_r4 {
                t.group_id == 1884
            } else {
                matches!(t.group_id, 1884 | 1920..=1923)
            };
            t.published
                && group_ok
                && !t.name.contains("Compressed")
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
    let idx0 = (h as usize) % base_moon_ores.len();
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
                && (t.name.starts_with("Bountiful ")
                    || t.name.starts_with("Shining ")
                    || t.name.starts_with("Resplendent ")
                    || t.name.starts_with("Sparkling ")
                    || t.name.starts_with("Dazzling "))
        })
        .map(|t| t.type_id)
        .unwrap_or(base_ore_tid)
}

/// Computes gas decompression efficiency in a structure from SDE attributes:
/// - `3262` (`gasDecompressionBaseEfficiency` = 0.80)
/// - `3261` (`structureGasDecompressionEfficiencyBonus`: +0.04 on Athanor 35835, +0.10 on Tatara 35836, 0.0 default)
/// - `3260` (`GasDecompressionEfficiencyBonus` = +1% per level of skill `62452` `Gas Decompression Efficiency`)
fn compute_gas_decompression_efficiency(
    ctx: &ServerContext,
    char_id: i64,
    structure_id_opt: Option<i64>,
) -> f64 {
    let base_eff = ctx
        .sde
        .attributes
        .get(&3262)
        .map(|a| a.default_value)
        .unwrap_or(0.80);
    let skill_bonus_pct_per_lvl = ctx
        .sde
        .types
        .get(&62452)
        .and_then(|t| t.default_attributes.get(&3260).copied())
        .unwrap_or(1.0);
    ctx.db.with_snapshot(|snap| {
        let skill_lvl = snap
            .skills
            .get(&char_id)
            .and_then(|m| m.get(&62452))
            .map(|s| s.level as f64)
            .unwrap_or(0.0);
        let struct_bonus = structure_id_opt
            .and_then(|sid| snap.structures.get(&sid))
            .and_then(|st| ctx.sde.types.get(&st.type_id))
            .and_then(|t| t.default_attributes.get(&3261).copied())
            .unwrap_or(0.0);
        let eff = base_eff + struct_bonus + skill_lvl * (skill_bonus_pct_per_lvl / 100.0);
        (eff * 10_000.0).round() / 10_000.0
    })
}

/// Computes the Moon Materials (`group_id == 427`, e.g. `16633..=16653` from `sde.type_materials`)
/// harvested by a Metenox Moon Drill (`30,000 m³/hour` raw moon volume × `efficiency` = `0.40`)
/// over `cycles` hours for the moon's ore composition (`derive_moon_ores_from_sde`).
/// DERIVED: Metenox Moon Drill harvests `30,000 m³/hour` of the moon's composition at `40%` efficiency
/// and outputs only `group_id == 427` Moon Materials (`docs/eve-wiki/Metenox_Moon_Drill.md:15,30`).
fn derive_metenox_moon_materials(
    sde: &eve_sde::SdeRegistry,
    solar_system_id: u32,
    moon_id: i64,
    efficiency: f64,
    cycles: u32,
) -> Vec<(TypeId, u64)> {
    let composition = derive_moon_ores_from_sde(sde, solar_system_id, moon_id);
    let mut totals: std::collections::BTreeMap<TypeId, f64> = std::collections::BTreeMap::new();
    for (ore_tid, frac) in composition {
        let portion_vol = sde
            .types
            .get(&ore_tid)
            .map(|t| t.volume.max(0.1) * (t.portion_size.max(1) as f64))
            .unwrap_or(1_000.0);
        let portions = (30_000.0 * frac * (cycles as f64)) / portion_vol;
        if let Some(mats) = sde.type_materials.get(&ore_tid) {
            for mat in mats {
                let is_moon_material = sde
                    .types
                    .get(&mat.material_type_id)
                    .is_some_and(|t| t.group_id == 427);
                if is_moon_material {
                    *totals.entry(mat.material_type_id).or_insert(0.0) +=
                        portions * (mat.quantity as f64) * efficiency;
                }
            }
        }
    }
    let mut out: Vec<(TypeId, u64)> = totals
        .into_iter()
        .map(|(tid, qty)| (tid, qty.round() as u64))
        .filter(|&(_, qty)| qty > 0)
        .collect();
    if out.is_empty() {
        let fallback_qty = (30.0 * 65.0 * efficiency * (cycles as f64)).round() as u64;
        out.push((16633, fallback_qty.max(1)));
    }
    out
}

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

            let Some(structure_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| _kwargs.get("structureID").and_then(|v| v.as_i64()))
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "StartNewExtraction requires structureID".into(),
                }));
            };

            // Server-authoritative extraction timings (`docs/eve-wiki/Moon_mining.md:107-112`):
            // duration must be in 6d..=56d (`518_400..=4_838_400` seconds, default 6d = 518,400s),
            // natural decay window fixed at 48h (172,800s) after chunk arrival.
            let now = ctx.now_unix();
            let explicit_dur = args
                .get(1)
                .and_then(|a| a.as_i64())
                .or_else(|| _kwargs.get("duration").and_then(|v| v.as_i64()))
                .or_else(|| _kwargs.get("chunkArrivalTime").and_then(|v| v.as_i64()));
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
                    let ore_list: Vec<PyRep> = ores
                        .into_iter()
                        .map(|(tid, pct)| {
                            PyRep::keyval([
                                ("typeID", PyRep::Integer(tid as i64)),
                                ("yield", PyRep::Real(pct)),
                            ])
                        })
                        .collect();

                    Some(Ok(PyRep::keyval([
                        ("structureID", PyRep::Integer(s_id)),
                        ("moonID", PyRep::Integer(m_id)),
                        ("extractionStartTime", PyRep::Integer(st as i64)),
                        ("chunkArrivalTime", PyRep::Integer(arr as i64)),
                        ("naturalDecayTime", PyRep::Integer(dec as i64)),
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

            let Some(structure_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| _kwargs.get("structureID").and_then(|v| v.as_i64()))
            else {
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
                ext.status = "Cancelled".into();
                Ok(true)
            });

            match res {
                Ok(_) => Some(Ok(PyRep::Bool(true))),
                Err(e) => Some(Err(e)),
            }
        }

        ("moonExtractions", "FractureChunkWithStructure") => {
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
                    message: "FractureChunkWithStructure requires structureID".into(),
                }));
            };

            let now = ctx.now_unix();
            // DERIVED: 15% jackpot probability roll via the context mechanic RNG (seeded test RNG).
            let is_jackpot = ctx.mechanic_rng.roll(0.15);

            // Atomically validate structure ownership, active extraction status, and chunk arrival
            // BEFORE spawning any asteroids so repeated calls cannot refill or duplicate moon ore.
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

                let caller_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0);
                if owner_corp_id != caller_corp {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Character lacks corporation permission to fracture chunk".into(),
                    });
                }

                let Some(ext) = snap.moon_extractions.get_mut(&structure_id) else {
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
                        message: format!(
                            "Moon chunk has not yet arrived (now={now}, arrival={})",
                            ext.chunk_arrival_unix
                        ),
                    });
                }

                let base_ores = if ext.moon_ores.is_empty() {
                    derive_moon_ores_from_sde(&ctx.sde, sys_id, ext.moon_id)
                } else {
                    ext.moon_ores.clone()
                };

                // Moon extraction chunk volume scales linearly at 30,000 m³/hour (`docs/eve-wiki/Moon_mining.md:107-112`)
                let duration_secs = ext
                    .chunk_arrival_unix
                    .saturating_sub(ext.extraction_start_unix)
                    .max(518_400);
                let total_chunk_volume_m3 = (duration_secs as f64 / 3600.0) * 30_000.0;

                let base_pos = DVec3::new(st_pos[0], st_pos[1], st_pos[2]);
                let mut spawned_specs = Vec::with_capacity(base_ores.len());
                let mut spawned_ids = Vec::with_capacity(base_ores.len());

                for (idx, &(base_tid, frac)) in base_ores.iter().enumerate() {
                    let ore_tid = if is_jackpot {
                        find_jackpot_variant(&ctx.sde, base_tid)
                    } else {
                        base_tid
                    };
                    let uvol = ctx
                        .sde
                        .types
                        .get(&ore_tid)
                        .map(|t| t.volume.max(0.1))
                        .unwrap_or(10.0);
                    let units = (((total_chunk_volume_m3 * frac) / uvol).round() as u64).max(1);
                    snap.next_entity_id += 1;
                    let ast_id = 950_000_000 + snap.next_entity_id;
                    let offset = DVec3::new(
                        (idx as f64 + 1.0) * 4_000.0,
                        (idx as f64) * 2_500.0,
                        -(idx as f64 + 1.0) * 3_500.0,
                    );
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

                let ext_mut = snap.moon_extractions.get_mut(&structure_id).unwrap();
                ext_mut.status = "Fractured".into();
                ext_mut.spawned_asteroid_ids = spawned_ids.clone();

                Ok((sys_id, spawned_ids, spawned_specs))
            });

            match commit_res {
                Ok((sys_id, ast_ids, spawned_specs)) => {
                    let handle = ctx.systems.get_or_spawn(sys_id);
                    let mut ore_tids = Vec::with_capacity(spawned_specs.len());
                    for (ast_id, ore_tid, units, uvol, pos) in spawned_specs {
                        ore_tids.push(PyRep::Integer(ore_tid as i64));
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
                    let ast_id_reps: Vec<PyRep> = ast_ids.iter().map(|&id| PyRep::Integer(id)).collect();
                    Some(Ok(PyRep::keyval([
                        ("structureID", PyRep::Integer(structure_id)),
                        ("fractured", PyRep::Bool(true)),
                        ("status", PyRep::String("Fractured".into())),
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

            let corp_id = args
                .first()
                .and_then(|a| a.as_i64())
                .unwrap_or_else(|| {
                    ctx.db.with_snapshot(|snap| {
                        snap.characters
                            .get(&char_id)
                            .map(|c| c.corporation_id)
                            .unwrap_or(0)
                    })
                });

            let now = ctx.now_unix();
            let extractions = ctx.db.with_snapshot(|snap| {
                snap.moon_extractions
                    .values()
                    .filter(|ext| {
                        snap.structures
                            .get(&ext.structure_id)
                            .map(|s| s.owner_corp_id == corp_id)
                            .unwrap_or(false)
                    })
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
                        ("readyToFracture", PyRep::Bool(ready)),
                    ])
                })
                .collect();

            Some(Ok(PyRep::List(list)))
        }

        ("moonExtractions", "GetMoonResourcesForStructure") => {
            let Some(structure_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| _kwargs.get("structureID").and_then(|v| v.as_i64()))
            else {
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
            let Some(structure_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| _kwargs.get("structureID").and_then(|v| v.as_i64()))
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetExtractionAndEventIDFromStructureID requires structureID".into(),
                }));
            };

            let now = ctx.now_unix();
            let ext = ctx
                .db
                .with_snapshot(|snap| snap.moon_extractions.get(&structure_id).cloned());

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
                Some(Ok(PyRep::keyval([
                    ("structureID", PyRep::Integer(e.structure_id)),
                    ("moonID", PyRep::Integer(e.moon_id)),
                    ("extractionStartTime", PyRep::Integer(e.extraction_start_unix as i64)),
                    ("chunkArrivalTime", PyRep::Integer(e.chunk_arrival_unix as i64)),
                    ("naturalDecayTime", PyRep::Integer(e.natural_decay_unix as i64)),
                    ("status", PyRep::String(effective_status)),
                    ("eventID", PyRep::Integer(e.event_id)),
                    ("activeAsteroidCount", PyRep::Integer(active_count)),
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
            // DERIVED: Metenox Moon Drill consumes Magmatic Gas (type 81143) per hourly cycle (`docs/eve-wiki/Metenox_Moon_Drill.md:15,30`).
            let gas_per_cycle: u32 = 10;

            let now = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(st) = snap.structures.get(&structure_id) else {
                    return Err(BridgeRpcError {
                        code: "StructureNotFound".into(),
                        message: format!("Structure {structure_id} not found"),
                    });
                };

                if !matches!(st.type_id, 81826 | 81824) {
                    return Err(BridgeRpcError {
                        code: "InvalidStructureType".into(),
                        message: format!(
                            "Auto moon mining requires a Metenox Moon Drill (type 81826), got structure type {}",
                            st.type_id
                        ),
                    });
                }

                let caller_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0);
                if st.owner_corp_id != caller_corp {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Character lacks corporation permissions for auto moon mining".into(),
                    });
                }

                let st_sys_id = st.solar_system_id;
                let sec = ctx
                    .sde
                    .solar_systems
                    .get(&st_sys_id)
                    .map(|s| s.security_status)
                    .unwrap_or(0.0);
                if sec >= 0.45 && st_sys_id < 31_000_000 {
                    return Err(BridgeRpcError {
                        code: "InvalidSystemSecurity".into(),
                        message: format!(
                            "Metenox Moon Drill cannot operate in High-Sec system {st_sys_id} (security {sec:.2}); requires Low-Sec (< 0.45), Null-Sec, or Wormhole space"
                        ),
                    });
                }

                let st_pos = st.position;
                let st_fuel_blocks = st.fuel_blocks;
                let item_fuel_blocks: u32 = snap
                    .items
                    .values()
                    .filter(|it| {
                        it.location_id == structure_id
                            && it.quantity > 0
                            && it.flag != InventoryFlag::Locked
                            && !snap.is_item_locked_in_contract_or_job(it.item_id)
                            && ctx
                                .sde
                                .types
                                .get(&it.type_id)
                                .is_some_and(|t| t.group_id == 1136)
                    })
                    .map(|it| it.quantity as u32)
                    .sum();
                let total_struct_fuel = st_fuel_blocks.saturating_add(item_fuel_blocks);
                let total_struct_gas: u32 = snap
                    .items
                    .values()
                    .filter(|it| {
                        it.location_id == structure_id
                            && it.quantity > 0
                            && it.flag != InventoryFlag::Locked
                            && !snap.is_item_locked_in_contract_or_job(it.item_id)
                            && it.type_id == 81143
                    })
                    .map(|it| it.quantity as u32)
                    .sum();

                let moon_id = resolve_structure_moon_id(&ctx.sde, st_sys_id, st_pos)
                    .unwrap_or(40_000_001);
                let hourly_materials =
                    derive_metenox_moon_materials(&ctx.sde, st_sys_id, moon_id, 0.40, 1);
                let hourly_yield: u64 = hourly_materials.iter().map(|&(_, q)| q as u64).sum();
                let primary_mat_tid = hourly_materials
                    .first()
                    .map(|&(tid, _)| tid)
                    .unwrap_or(16633);

                let auto_miner = snap.auto_moon_miners.entry(structure_id).or_insert_with(|| {
                    AutoMoonMinerRecord {
                        structure_id,
                        fuel_units_remaining: total_struct_fuel,
                        magmatic_gas_remaining: total_struct_gas,
                        efficiency: 0.40, // 40% Metenox Moon Drill baseline (`docs/eve-wiki/Metenox_Moon_Drill.md:15,30`)
                        cycle_duration_seconds: 3600,
                        last_cycle_unix: now,
                        ore_type_id: primary_mat_tid,
                        hourly_yield,
                        status: "Active".into(),
                    }
                });
                // Authoritative sync from structure inventory (never preserve stale record balances via `.max()`):
                auto_miner.fuel_units_remaining = total_struct_fuel;
                auto_miner.magmatic_gas_remaining = total_struct_gas;
                auto_miner.hourly_yield = hourly_yield;
                auto_miner.ore_type_id = primary_mat_tid;
                auto_miner.status = if total_struct_fuel >= fuel_per_cycle && total_struct_gas >= gas_per_cycle {
                    "Active".into()
                } else if total_struct_fuel < fuel_per_cycle {
                    "OutOfFuel".into()
                } else {
                    "OutOfMagmaticGas".into()
                };

                Ok((auto_miner.clone(), hourly_materials))
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
            let gas_per_cycle: u32 = 10;

            let now = ctx.now_unix();
            let date = eve_db::unix_secs_to_date_string(now);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let (st_type_id, st_owner_corp, st_sys_id, st_pos, st_fuel_blocks) = {
                    let Some(st) = snap.structures.get(&structure_id) else {
                        return Err(BridgeRpcError {
                            code: "StructureNotFound".into(),
                            message: format!("Structure {structure_id} not found"),
                        });
                    };
                    (
                        st.type_id,
                        st.owner_corp_id,
                        st.solar_system_id,
                        st.position,
                        st.fuel_blocks,
                    )
                };

                if !matches!(st_type_id, 81826 | 81824) {
                    return Err(BridgeRpcError {
                        code: "InvalidStructureType".into(),
                        message: format!(
                            "Auto moon mining requires a Metenox Moon Drill (type 81826), got structure type {st_type_id}"
                        ),
                    });
                }

                let caller_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0);
                if st_owner_corp != caller_corp {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Character lacks corporation permissions for auto moon mining".into(),
                    });
                }

                let sec = ctx
                    .sde
                    .solar_systems
                    .get(&st_sys_id)
                    .map(|s| s.security_status)
                    .unwrap_or(0.0);
                if sec >= 0.45 && st_sys_id < 31_000_000 {
                    return Err(BridgeRpcError {
                        code: "InvalidSystemSecurity".into(),
                        message: format!(
                            "Metenox Moon Drill cannot operate in High-Sec system {st_sys_id} (security {sec:.2}); requires Low-Sec (< 0.45), Null-Sec, or Wormhole space"
                        ),
                    });
                }

                let item_fuel_blocks: u32 = snap
                    .items
                    .values()
                    .filter(|it| {
                        it.location_id == structure_id
                            && it.quantity > 0
                            && it.flag != InventoryFlag::Locked
                            && !snap.is_item_locked_in_contract_or_job(it.item_id)
                            && ctx
                                .sde
                                .types
                                .get(&it.type_id)
                                .is_some_and(|t| t.group_id == 1136)
                    })
                    .map(|it| it.quantity as u32)
                    .sum();
                let total_struct_fuel = st_fuel_blocks.saturating_add(item_fuel_blocks);
                let total_struct_gas: u32 = snap
                    .items
                    .values()
                    .filter(|it| {
                        it.location_id == structure_id
                            && it.quantity > 0
                            && it.flag != InventoryFlag::Locked
                            && !snap.is_item_locked_in_contract_or_job(it.item_id)
                            && it.type_id == 81143
                    })
                    .map(|it| it.quantity as u32)
                    .sum();
                let moon_id = resolve_structure_moon_id(&ctx.sde, st_sys_id, st_pos)
                    .unwrap_or(40_000_001);
                let hourly_materials =
                    derive_metenox_moon_materials(&ctx.sde, st_sys_id, moon_id, 0.40, 1);
                let hourly_yield: u64 = hourly_materials.iter().map(|&(_, q)| q as u64).sum();
                let primary_mat_tid = hourly_materials
                    .first()
                    .map(|&(tid, _)| tid)
                    .unwrap_or(16633);

                let (cycles, efficiency, fuel_req, gas_req, fuel_rem) = {
                    let auto_miner = snap.auto_moon_miners.entry(structure_id).or_insert_with(|| {
                        AutoMoonMinerRecord {
                            structure_id,
                            fuel_units_remaining: total_struct_fuel,
                            magmatic_gas_remaining: total_struct_gas,
                            efficiency: 0.40,
                            cycle_duration_seconds: 3600,
                            last_cycle_unix: now,
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

                // Deduct consumed fuel blocks from structure fuel bay / unlocked items
                let mut rem_fuel_to_deduct = fuel_req;
                if let Some(st_mut) = snap.structures.get_mut(&structure_id) {
                    let from_bay = st_mut.fuel_blocks.min(rem_fuel_to_deduct);
                    st_mut.fuel_blocks -= from_bay;
                    rem_fuel_to_deduct -= from_bay;
                }
                if rem_fuel_to_deduct > 0 {
                    let mut fuel_item_ids: Vec<i64> = snap
                        .items
                        .values()
                        .filter(|it| {
                            it.location_id == structure_id
                                && it.quantity > 0
                                && it.flag != InventoryFlag::Locked
                                && !snap.is_item_locked_in_contract_or_job(it.item_id)
                                && ctx
                                    .sde
                                    .types
                                    .get(&it.type_id)
                                    .is_some_and(|t| t.group_id == 1136)
                        })
                        .map(|it| it.item_id)
                        .collect();
                    fuel_item_ids.sort_unstable();
                    for fid in fuel_item_ids {
                        if rem_fuel_to_deduct == 0 {
                            break;
                        }
                        if let Some(fit) = snap.items.get_mut(&fid) {
                            let take = (fit.quantity.max(0) as u32).min(rem_fuel_to_deduct);
                            fit.quantity -= take as i32;
                            rem_fuel_to_deduct -= take;
                            if fit.quantity <= 0 {
                                snap.items.remove(&fid);
                            }
                        }
                    }
                }
                if rem_fuel_to_deduct > 0 {
                    return Err(BridgeRpcError {
                        code: "OutOfFuel".into(),
                        message: format!(
                            "Insufficient fuel blocks in structure {structure_id} to complete deduction ({rem_fuel_to_deduct} short)"
                        ),
                    });
                }

                // Deduct consumed Magmatic Gas (type 81143) from unlocked structure items
                let mut rem_gas_to_deduct = gas_req;
                let mut gas_item_ids: Vec<i64> = snap
                    .items
                    .values()
                    .filter(|it| {
                        it.location_id == structure_id
                            && it.quantity > 0
                            && it.flag != InventoryFlag::Locked
                            && !snap.is_item_locked_in_contract_or_job(it.item_id)
                            && it.type_id == 81143
                    })
                    .map(|it| it.item_id)
                    .collect();
                gas_item_ids.sort_unstable();
                for gid in gas_item_ids {
                    if rem_gas_to_deduct == 0 {
                        break;
                    }
                    if let Some(git) = snap.items.get_mut(&gid) {
                        let take = (git.quantity.max(0) as u32).min(rem_gas_to_deduct);
                        git.quantity -= take as i32;
                        rem_gas_to_deduct -= take;
                        if git.quantity <= 0 {
                            snap.items.remove(&gid);
                        }
                    }
                }
                if rem_gas_to_deduct > 0 {
                    return Err(BridgeRpcError {
                        code: "OutOfMagmaticGas".into(),
                        message: format!(
                            "Insufficient Magmatic Gas (type 81143) in structure {structure_id} to complete deduction ({rem_gas_to_deduct} short)"
                        ),
                    });
                }

                // Compute harvested Moon Materials (`group_id == 427`, e.g. `16633..=16653`) for `cycles` hours
                let cycle_materials =
                    derive_metenox_moon_materials(&ctx.sde, st_sys_id, moon_id, efficiency, cycles);
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

        // ====================================================================
        // MN-15: Corp Mining Ledger Service (corpMiningLedger.*)
        // ====================================================================
        ("corpMiningLedger", "GetObserversWithMiningEvents") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let caller_corp = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0)
            });

            if caller_corp == 0 {
                return Some(Err(BridgeRpcError {
                    code: "NoCorporation".into(),
                    message: "Character does not belong to a corporation".into(),
                }));
            }

            let observers = ctx.db.with_snapshot(|snap| {
                let mut list = Vec::new();
                for (&obs_id, entries) in &snap.observer_mining_ledger {
                    if let Some(st) = snap.structures.get(&obs_id) {
                        if st.owner_corp_id == caller_corp && !entries.is_empty() {
                            let last_date = entries
                                .last()
                                .map(|e| e.last_updated_date.clone())
                                .unwrap_or_else(|| "2026-10-19".into());
                            list.push((obs_id, "structure".to_string(), last_date));
                        }
                    }
                }
                list
            });

            let descriptor = DBRowDescriptor::new([
                ("observerID", DbColumnType::I8),
                ("observerType", DbColumnType::Str),
                ("lastUpdated", DbColumnType::Str),
            ]);

            let rows: Vec<Vec<PyRep>> = observers
                .into_iter()
                .map(|(id, o_type, date)| {
                    vec![
                        PyRep::Integer(id),
                        PyRep::String(o_type),
                        PyRep::String(date),
                    ]
                })
                .collect();

            Some(Ok(descriptor.build_crowset(rows)))
        }

        ("corpMiningLedger", "GetObserverLedger") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let Some(observer_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| _kwargs.get("observerID").and_then(|v| v.as_i64()))
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetObserverLedger requires observerID".into(),
                }));
            };

            let res = ctx.db.with_snapshot(|snap| {
                let caller_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0);

                if let Some(st) = snap.structures.get(&observer_id) {
                    if st.owner_corp_id != caller_corp {
                        return Err(BridgeRpcError {
                            code: "AccessDenied".into(),
                            message: format!(
                                "Character {char_id} (corp {caller_corp}) lacks ACL access to observer {observer_id} (owner corp {})",
                                st.owner_corp_id
                            ),
                        });
                    }
                }

                let entries = snap
                    .observer_mining_ledger
                    .get(&observer_id)
                    .cloned()
                    .unwrap_or_default();

                Ok(entries)
            });

            match res {
                Ok(entries) => {
                    let descriptor = DBRowDescriptor::new([
                        ("characterID", DbColumnType::I8),
                        ("recordedCorporationID", DbColumnType::I8),
                        ("typeID", DbColumnType::I4),
                        ("quantity", DbColumnType::I8),
                        ("lastUpdated", DbColumnType::Str),
                    ]);

                    let rows: Vec<Vec<PyRep>> = entries
                        .into_iter()
                        .map(|e| {
                            vec![
                                PyRep::Integer(e.character_id),
                                PyRep::Integer(e.recorded_corporation_id),
                                PyRep::Integer(e.type_id as i64),
                                PyRep::Integer(e.quantity),
                                PyRep::String(e.last_updated_date),
                            ]
                        })
                        .collect();

                    Some(Ok(descriptor.build_crowset(rows)))
                }
                Err(e) => Some(Err(e)),
            }
        }

        // ====================================================================
        // MN-16: In-Space & Structure Compression & Decompression
        // ====================================================================
        ("inSpaceCompressionMgr", "CompressItemInSpace") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "MustBeInSpace".into(),
                    message: "CompressItemInSpace requires being undocked in space".into(),
                }));
            }

            let Some(item_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CompressItemInSpace requires itemID".into(),
                }));
            };

            let ship_id = session.ship_id;
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeInSpace".into(),
                    message: "CompressItemInSpace requires a valid solarSystemID in space".into(),
                }));
            };

            // 1. Validate target item in ship's mining hold and determine its SDE attributes
            let item_check = ctx.db.with_snapshot(|snap| {
                let Some(item) = snap.items.get(&item_id) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Item {item_id} not found"),
                    });
                };
                if item.owner_id != char_id || Some(item.location_id) != ship_id {
                    return Err(BridgeRpcError {
                        code: "CompressionFailed".into(),
                        message: "Item must be owned by character and located on the active ship".into(),
                    });
                }
                if item.flag == InventoryFlag::Locked
                    || snap.is_item_locked_in_contract_or_job(item_id)
                {
                    return Err(BridgeRpcError {
                        code: "ItemLocked".into(),
                        message: "Item is locked in a contract or job and cannot be compressed".into(),
                    });
                }
                if item.quantity < 1 {
                    return Err(BridgeRpcError {
                        code: "CompressionFailed".into(),
                        message: "Item has invalid quantity".into(),
                    });
                }
                // V24 in-space compression requires the material to be in the ship's mining hold (`docs/eve-wiki/Compression.md:27`)
                let in_mining_hold = matches!(
                    item.flag,
                    InventoryFlag::SpecializedOreHold
                        | InventoryFlag::SpecializedIceHold
                        | InventoryFlag::SpecializedGasHold
                        | InventoryFlag::SpecializedMineralHold
                );
                if !in_mining_hold {
                    return Err(BridgeRpcError {
                        code: "NotInMiningHold".into(),
                        message: "Material to compress in space must be in the ship's mining hold".into(),
                    });
                }
                let Some(&comp_type) = ctx.sde.compressible_types.get(&item.type_id) else {
                    return Err(BridgeRpcError {
                        code: "CompressionFailed".into(),
                        message: format!("Type {} is not compressible", item.type_id),
                    });
                };
                let Some(item_t) = ctx.sde.types.get(&item.type_id) else {
                    return Err(BridgeRpcError {
                        code: "CompressionFailed".into(),
                        message: format!("Unknown SDE type {}", item.type_id),
                    });
                };
                Ok((comp_type, item_t.type_id))
            });
            let (comp_type, item_type_id) = match item_check {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let logistics_bonus_pct_per_lvl = ctx
                .sde
                .types
                .get(&62453)
                .and_then(|t| t.default_attributes.get(&3263).copied())
                .unwrap_or(10.0);

            // 2. Identify candidate compressor ships in the same solar system (self or fleet members)
            // whose active compressor module (`group_id == 4174`) matches the item's SDE attributes,
            // satisfies SDE hull restrictions (`1302..=1305` `canFitShipType1..4` and `1298..=1301` `canFitShipGroup01..04`),
            // and satisfies SDE `3265` (`activationRequiresActiveIndustrialCore`) via an active `group_id == 515` module.
            let fittings_guard = ctx
                .ship_fittings
                .read()
                .map(|g| g.clone())
                .unwrap_or_default();
            let candidate_compressors: Vec<(i64, f64)> = ctx.db.with_snapshot(|snap| {
                let mut candidate_chars = vec![char_id];
                for fl in snap.fleets.values() {
                    if fl.members.contains(&char_id) {
                        for &m_id in &fl.members {
                            if !candidate_chars.contains(&m_id) {
                                candidate_chars.push(m_id);
                            }
                        }
                    }
                }
                candidate_chars
                    .into_iter()
                    .filter_map(|cid| {
                        let ch = snap.characters.get(&cid)?;
                        if ch.solar_system_id != sys_id
                            || ch.station_id.is_some()
                            || ch.structure_id.is_some()
                        {
                            return None;
                        }
                        let c_ship_id = ch.active_ship_id;
                        let c_ship_type = snap.items.get(&c_ship_id)?.type_id;
                        let c_ship_group = ctx
                            .sde
                            .types
                            .get(&c_ship_type)
                            .map(|t| t.group_id)
                            .unwrap_or(0);
                        let ship_fit_opt = fittings_guard.get(&c_ship_id);

                        let is_mod_active = |mod_item: &InventoryItemRecord| -> bool {
                            let active_in_fit = ship_fit_opt
                                .and_then(|f| f.modules.get(&mod_item.item_id))
                                .is_some_and(|fm| {
                                    matches!(
                                        fm.state,
                                        eve_dogma::ModuleState::Active
                                            | eve_dogma::ModuleState::Overloaded
                                    )
                                });
                            let active_in_db = snap.module_active_states.contains(&mod_item.item_id);
                            active_in_fit || active_in_db
                        };

                        let matches_hull_sde = |mod_t: &eve_sde::InvType| -> bool {
                            let allowed_types: Vec<u32> = [1302u16, 1303, 1304, 1305]
                                .into_iter()
                                .filter_map(|aid| mod_t.default_attributes.get(&aid).copied())
                                .filter(|&v| v > 0.0)
                                .map(|v| v.round() as u32)
                                .collect();
                            let allowed_groups: Vec<u32> = [1298u16, 1299, 1300, 1301]
                                .into_iter()
                                .filter_map(|aid| mod_t.default_attributes.get(&aid).copied())
                                .filter(|&v| v > 0.0)
                                .map(|v| v.round() as u32)
                                .collect();
                            if allowed_types.is_empty() && allowed_groups.is_empty() {
                                true
                            } else {
                                allowed_types.contains(&c_ship_type)
                                    || allowed_groups.contains(&c_ship_group)
                            }
                        };

                        let has_active_industrial_core = snap.items.values().any(|it| {
                            if it.location_id != c_ship_id
                                || !(27..=34).contains(&(it.flag as u16))
                                || !is_mod_active(it)
                            {
                                return false;
                            }
                            let Some(core_t) = ctx.sde.types.get(&it.type_id) else {
                                return false;
                            };
                            core_t.group_id == 515 && matches_hull_sde(core_t)
                        });

                        let logistics_lvl = snap
                            .skills
                            .get(&cid)
                            .and_then(|sk| sk.get(&62453))
                            .map(|r| r.level.clamp(0, 5) as f64)
                            .unwrap_or(0.0);
                        let mut best_range: Option<f64> = None;
                        for it in snap.items.values() {
                            if it.location_id != c_ship_id
                                || !(27..=34).contains(&(it.flag as u16))
                                || !is_mod_active(it)
                            {
                                continue;
                            }
                            let Some(mod_t) = ctx.sde.types.get(&it.type_id) else {
                                continue;
                            };
                            if mod_t.group_id != 4174 {
                                continue;
                            }
                            // Validate hull restriction solely from SDE `canFitShipType1..4` (`1302..=1305`) and `canFitShipGroup01..04` (`1298..=1301`)
                            if !matches_hull_sde(mod_t) {
                                continue;
                            }
                            // Enforce SDE `3265` (`activationRequiresActiveIndustrialCore`) (`docs/eve-wiki/Compression.md:39`)
                            let requires_core = mod_t
                                .default_attributes
                                .get(&3265)
                                .copied()
                                .unwrap_or(0.0)
                                > 0.0;
                            if requires_core && !has_active_industrial_core {
                                continue;
                            }
                            // Resolve compressor SDE attribute `3255` (`compressibleItemsTypeList`) against the client
                            // type list (`res:/staticdata/typelist.fsdbinary`, imported as `sde_type_lists`).
                            let type_list_id = mod_t
                                .default_attributes
                                .get(&3255)
                                .copied()
                                .unwrap_or(0.0)
                                .round() as u32;
                            let matches_category = ctx
                                .sde
                                .type_lists
                                .get(&type_list_id)
                                .zip(ctx.sde.types.get(&item_type_id))
                                .is_some_and(|(tl, item_t)| tl.contains(item_t));
                            if !matches_category {
                                continue;
                            }
                            let base_range = mod_t
                                .default_attributes
                                .get(&54)
                                .copied()
                                .unwrap_or(44_000.0);
                            let eff_range = base_range
                                * (1.0 + (logistics_bonus_pct_per_lvl / 100.0) * logistics_lvl);
                            best_range = Some(best_range.map_or(eff_range, |r| r.max(eff_range)));
                        }
                        best_range.map(|r| (c_ship_id, r))
                    })
                    .collect()
            });

            // 3. Verify 3D Ballpark distance to at least one matching active compressor ship
            let handle = ctx.systems.get_or_spawn(sys_id);
            let my_pos = if let Some(sid) = ship_id {
                handle
                    .get_ball_snapshot(sid)
                    .await
                    .ok()
                    .flatten()
                    .map(|(_, b)| b.position)
            } else {
                None
            };
            let mut in_range_compressor = false;
            for (c_ship_id, max_range) in candidate_compressors {
                if Some(c_ship_id) == ship_id {
                    in_range_compressor = true;
                    break;
                }
                if let Some(mpos) = my_pos {
                    if let Ok(Some((_, cball))) = handle.get_ball_snapshot(c_ship_id).await {
                        if (mpos - cball.position).length() <= max_range {
                            in_range_compressor = true;
                            break;
                        }
                    }
                }
            }

            if !in_range_compressor {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveCompressorInRange".into(),
                    message: "In-space compression requires a ship with an active Industrial Core and matching active Compressor module in range".into(),
                }));
            }

            // 4. V24 1:1 lossless unit compression (`sde_compressible_types`: 1 raw unit -> 1 compressed unit)
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if snap.is_item_locked_in_contract_or_job(item_id) {
                    return Err(BridgeRpcError {
                        code: "ItemLocked".into(),
                        message: "Item is locked in a contract or job and cannot be compressed".into(),
                    });
                }
                let Some(item) = snap.items.get_mut(&item_id) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Item {item_id} not found"),
                    });
                };
                if item.owner_id != char_id
                    || Some(item.location_id) != ship_id
                    || item.flag == InventoryFlag::Locked
                    || item.quantity < 1
                {
                    return Err(BridgeRpcError {
                        code: "CompressionFailed".into(),
                        message: "Item cannot be compressed in space".into(),
                    });
                }
                item.type_id = comp_type;
                item.version += 1;
                Ok((item.item_id, item.type_id, item.quantity, 0i32))
            });

            match res {
                Ok((iid, ctype, qty, rem)) => Some(Ok(PyRep::keyval([
                    ("itemID", PyRep::Integer(iid)),
                    ("compressedTypeID", PyRep::Integer(ctype as i64)),
                    ("quantity", PyRep::Integer(qty as i64)),
                    ("compressedUnits", PyRep::Integer(qty as i64)),
                    ("remainderUnits", PyRep::Integer(rem as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("structureCompressionMgr", "CompressItemInStructure") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            if session.station_id.is_none() && session.structure_id.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "CompressItemInStructure requires being docked at an Upwell structure".into(),
                }));
            }
            let Some(structure_id) = session.structure_id else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeInStructure".into(),
                    message: "CompressItemInStructure requires an Upwell structure with reprocessing, not an NPC station".into(),
                }));
            };

            // Args can be [structure_id, item_id] or [item_id]
            let item_id = if args.len() >= 2 {
                if let Some(arg_sid) = args.first().and_then(|a| a.as_i64()) {
                    if arg_sid != structure_id {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidStructure".into(),
                            message: format!(
                                "Cannot compress using structure {arg_sid} while docked at {structure_id}"
                            ),
                        }));
                    }
                }
                args.get(1).and_then(|a| a.as_i64()).unwrap_or(0)
            } else {
                args.first().and_then(|a| a.as_i64()).unwrap_or(0)
            };

            if item_id == 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CompressItemInStructure requires itemID".into(),
                }));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(st) = snap.structures.get(&structure_id) else {
                    return Err(BridgeRpcError {
                        code: "StructureNotFound".into(),
                        message: format!("Structure {structure_id} not found"),
                    });
                };
                let is_refinery = ctx
                    .sde
                    .types
                    .get(&st.type_id)
                    .is_some_and(|t| t.group_id == 1406);
                let has_reproc_service = is_refinery
                    || snap.items.values().any(|it| {
                        it.location_id == structure_id
                            && (it.type_id == 35894
                                || ctx
                                    .sde
                                    .types
                                    .get(&it.type_id)
                                    .is_some_and(|t| t.group_id == 1322))
                    });
                if st.fuel_blocks == 0 || !has_reproc_service {
                    return Err(BridgeRpcError {
                        code: "ReprocessingServiceOffline".into(),
                        message: format!(
                            "Structure {structure_id} does not have an online Reprocessing service"
                        ),
                    });
                }

                if snap.is_item_locked_in_contract_or_job(item_id) {
                    return Err(BridgeRpcError {
                        code: "ItemLocked".into(),
                        message: format!("Item {item_id} is locked in a contract or job"),
                    });
                }

                let Some(item) = snap.items.get_mut(&item_id) else {
                    return Err(BridgeRpcError {
                        code: "CompressionFailed".into(),
                        message: format!("Item {item_id} not found"),
                    });
                };
                if item.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Character does not own this item".into(),
                    });
                }
                if item.flag == InventoryFlag::Locked {
                    return Err(BridgeRpcError {
                        code: "ItemLocked".into(),
                        message: format!("Item {item_id} is locked"),
                    });
                }
                let reachable = (item.location_id == structure_id
                    && item.flag == InventoryFlag::Hangar)
                    || (Some(item.location_id) == session.ship_id);
                if !reachable || item.quantity < 1 {
                    return Err(BridgeRpcError {
                        code: "CompressionFailed".into(),
                        message: "Item cannot be compressed (invalid quantity or not in current structure hangar)".into(),
                    });
                }
                let Some(&comp_type) = ctx.sde.compressible_types.get(&item.type_id) else {
                    return Err(BridgeRpcError {
                        code: "CompressionFailed".into(),
                        message: format!("Type {} is not compressible", item.type_id),
                    });
                };
                let group_id = ctx
                    .sde
                    .types
                    .get(&item.type_id)
                    .map(|t| t.group_id)
                    .unwrap_or(0);
                // Gas (group 711) and Moon Ore (groups 1884, 1920..=1923) cannot be compressed in structures (`docs/eve-wiki/Compression.md:57-60`)
                if group_id == 711 || matches!(group_id, 1884 | 1920..=1923) {
                    return Err(BridgeRpcError {
                        code: "StructureCannotCompressType".into(),
                        message: "Gas and Moon Ore cannot be compressed in a structure; use an in-space compressor".into(),
                    });
                }

                // V24 1:1 lossless unit compression (`sde_compressible_types`: 1 raw unit -> 1 compressed unit)
                item.type_id = comp_type;
                item.version += 1;
                Ok((item.item_id, item.type_id, item.quantity, 0i32))
            });

            match res {
                Ok((iid, ctype, qty, rem)) => Some(Ok(PyRep::keyval([
                    ("itemID", PyRep::Integer(iid)),
                    ("compressedTypeID", PyRep::Integer(ctype as i64)),
                    ("quantity", PyRep::Integer(qty as i64)),
                    ("compressedUnits", PyRep::Integer(qty as i64)),
                    ("remainderUnits", PyRep::Integer(rem as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("structureCompressionMgr", "DecompressGasInStructure") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            if session.station_id.is_none() && session.structure_id.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "DecompressGasInStructure requires being docked at an Upwell structure".into(),
                }));
            }
            let Some(structure_id) = session.structure_id else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeInStructure".into(),
                    message: "DecompressGasInStructure requires an Upwell structure".into(),
                }));
            };

            let item_id = if args.len() >= 2 {
                if let Some(arg_sid) = args.first().and_then(|a| a.as_i64()) {
                    if arg_sid != structure_id {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidStructure".into(),
                            message: format!(
                                "Cannot decompress gas using structure {arg_sid} while docked at {structure_id}"
                            ),
                        }));
                    }
                }
                args.get(1).and_then(|a| a.as_i64()).unwrap_or(0)
            } else {
                args.first().and_then(|a| a.as_i64()).unwrap_or(0)
            };

            if item_id == 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DecompressGasInStructure requires itemID".into(),
                }));
            }

            // Always use `session.structure_id` exclusively (never client-supplied structure ID)
            let efficiency =
                compute_gas_decompression_efficiency(ctx, char_id, Some(structure_id));

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(st) = snap.structures.get(&structure_id) else {
                    return Err(BridgeRpcError {
                        code: "StructureNotFound".into(),
                        message: format!("Structure {structure_id} not found"),
                    });
                };
                let is_refinery = ctx
                    .sde
                    .types
                    .get(&st.type_id)
                    .is_some_and(|t| t.group_id == 1406);
                let has_reproc_service = is_refinery
                    || snap.items.values().any(|it| {
                        it.location_id == structure_id
                            && (it.type_id == 35894
                                || ctx
                                    .sde
                                    .types
                                    .get(&it.type_id)
                                    .is_some_and(|t| t.group_id == 1322))
                    });
                if st.fuel_blocks == 0 || !has_reproc_service {
                    return Err(BridgeRpcError {
                        code: "ReprocessingServiceOffline".into(),
                        message: format!(
                            "Structure {structure_id} does not have an online Reprocessing service"
                        ),
                    });
                }

                let is_locked_in_contract_or_job = snap.is_item_locked_in_contract_or_job(item_id);

                let (raw_tid, decomp_qty) = {
                    let Some(item) = snap.items.get_mut(&item_id) else {
                        return Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("Item {item_id} not found"),
                        });
                    };

                    if item.owner_id != char_id {
                        return Err(BridgeRpcError {
                            code: "AccessDenied".into(),
                            message: "Character does not own this item".into(),
                        });
                    }

                    if item.flag == InventoryFlag::Locked || is_locked_in_contract_or_job {
                        return Err(BridgeRpcError {
                            code: "ItemLocked".into(),
                            message: format!(
                                "Item {item_id} is locked (e.g. in courier escrow or contract) and cannot be decompressed"
                            ),
                        });
                    }

                    if item.location_id != structure_id || item.flag != InventoryFlag::Hangar {
                        return Err(BridgeRpcError {
                            code: "ItemNotInStructureHangar".into(),
                            message: format!(
                                "Item {item_id} must be in your hangar at structure {structure_id} to decompress"
                            ),
                        });
                    }

                    // Reverse lookup: find raw type that compresses to item.type_id
                    let raw_type = ctx
                        .sde
                        .compressible_types
                        .iter()
                        .find(|&(_, &c_tid)| c_tid == item.type_id)
                        .map(|(&r_tid, _)| r_tid);

                    let Some(raw_tid) = raw_type else {
                        return Err(BridgeRpcError {
                            code: "NotCompressedItem".into(),
                            message: format!("Type {} is not a compressed item", item.type_id),
                        });
                    };

                    // Only compressed gas (raw group_id == 711) can be decompressed (`docs/eve-wiki/Compression.md:62-66`)
                    let is_gas = ctx
                        .sde
                        .types
                        .get(&raw_tid)
                        .is_some_and(|t| t.group_id == 711);
                    if !is_gas {
                        return Err(BridgeRpcError {
                            code: "NotCompressedGas".into(),
                            message: format!(
                                "Type {} (raw type {raw_tid}) is not compressed harvestable cloud gas (group 711)",
                                item.type_id
                            ),
                        });
                    }

                    // V24 1:1 unit semantics: decompressed units = floor(quantity * efficiency)
                    let total_raw = ((item.quantity as f64) * efficiency).floor() as i64;
                    if total_raw <= 0 {
                        return Err(BridgeRpcError {
                            code: "InsufficientQuantityToDecompress".into(),
                            message: "Quantity too small to yield decompressed gas units".into(),
                        });
                    }
                    // Consume the compressed gas item
                    item.quantity = 0;
                    (raw_tid, total_raw)
                };

                // Remove 0-qty item
                snap.items.remove(&item_id);

                // Add decompressed gas item to structure hangar
                snap.next_entity_id += 1;
                let new_id = snap.next_entity_id;
                snap.items.insert(
                    new_id,
                    InventoryItemRecord {
                        item_id: new_id,
                        type_id: raw_tid,
                        owner_id: char_id,
                        location_id: structure_id,
                        flag: InventoryFlag::Hangar,
                        quantity: decomp_qty as i32,
                        singleton: false,
                        version: 1,
                        ..Default::default()
                    },
                );

                Ok((new_id, raw_tid, decomp_qty))
            });

            match res {
                Ok((new_iid, r_tid, d_qty)) => Some(Ok(PyRep::keyval([
                    ("itemID", PyRep::Integer(new_iid)),
                    ("decompressedTypeID", PyRep::Integer(r_tid as i64)),
                    ("decompressedQuantity", PyRep::Integer(d_qty)),
                    ("decompressedUnits", PyRep::Integer(d_qty)),
                    ("efficiency", PyRep::Real(efficiency)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("structureCompressionMgr", "GetMyGasDecompressionEfficiency") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if let Some(arg_sid) = args.first().and_then(|a| a.as_i64()) {
                if session.structure_id != Some(arg_sid) {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidStructure".into(),
                        message: format!(
                            "Cannot query gas decompression efficiency for structure {arg_sid} while not docked there"
                        ),
                    }));
                }
            }
            let eff = compute_gas_decompression_efficiency(ctx, char_id, session.structure_id);
            Some(Ok(PyRep::Real(eff)))
        }

        // ====================================================================
        // MN-18: Career Progression Verification from Mining Ledger
        // ====================================================================
        ("careerGoals", "GetMiningCareerProgress")
        | ("seasonManager", "get_mining_goal_progress") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let goal_id = args.first().and_then(|a| a.as_i64()).unwrap_or(2) as u32;

            let total_mined = ctx.db.with_snapshot(|snap| {
                snap.mining_ledger
                    .iter()
                    .filter(|r| r.character_id == char_id)
                    .map(|r| r.quantity.max(0) as u64)
                    .sum::<u64>()
            });

            Some(Ok(PyRep::keyval([
                ("goalID", PyRep::Integer(goal_id as i64)),
                ("currentProgress", PyRep::Integer(total_mined as i64)),
                ("verifiedFromLedger", PyRep::Bool(true)),
            ])))
        }

        _ => None,
    }
}
