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

/// Canonical R4 moon ores for deterministic moon composition when not specified in SDE:
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

            let now = ctx.now_unix();
            let (start_time, arrival_time, decay_time) = if args.len() >= 4 {
                let st = args.get(1).and_then(|a| a.as_i64()).unwrap_or(now as i64) as u64;
                let arr = args.get(2).and_then(|a| a.as_i64()).unwrap_or((now + 86_400) as i64) as u64;
                let dec = args.get(3).and_then(|a| a.as_i64()).unwrap_or((arr + 86_400) as i64) as u64;
                (st, arr, dec)
            } else if args.len() == 3 {
                let arr = args.get(1).and_then(|a| a.as_i64()).unwrap_or((now + 86_400) as i64) as u64;
                let dec = args.get(2).and_then(|a| a.as_i64()).unwrap_or((arr + 86_400) as i64) as u64;
                (now, arr, dec)
            } else if args.len() == 2 {
                let arr = args.get(1).and_then(|a| a.as_i64()).unwrap_or((now + 86_400) as i64) as u64;
                (now, arr, arr + 86_400)
            } else {
                (now, now + 86_400, now + 172_800)
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
                        message: format!(
                            "Character {char_id} (corp {caller_corp}) lacks permissions on structure {structure_id} (corp {})",
                            st.owner_corp_id
                        ),
                    });
                }

                // Deterministic moon celestial ID in the structure's system
                let moon_id = 40_000_000 + (st.solar_system_id as i64 % 1000) * 100 + 87;

                // Deterministic moon composition (R4/R8 moon ores)
                let moon_ores = vec![
                    (MOON_ORE_BITUMENS, 0.40),
                    (MOON_ORE_ZEOLITES, 0.35),
                    (MOON_ORE_COBALTITE, 0.25),
                ];

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

            let structure_info = ctx.db.with_snapshot(|snap| {
                snap.structures
                    .get(&structure_id)
                    .map(|st| (st.solar_system_id, st.position, st.owner_corp_id))
            });

            let Some((sys_id, st_pos, owner_corp_id)) = structure_info else {
                return Some(Err(BridgeRpcError {
                    code: "StructureNotFound".into(),
                    message: format!("Structure {structure_id} not found"),
                }));
            };

            let caller_corp = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0)
            });

            if owner_corp_id != caller_corp {
                return Some(Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "Character lacks corporation permission to fracture chunk".into(),
                }));
            }

            // Chunk explosion: spawn minable moon asteroids into the Ballpark near the structure
            let handle = ctx.systems.get_or_spawn(sys_id);

            let ores_to_spawn = vec![
                (MOON_ORE_BITUMENS, 5_000_u64, 10.0_f64),
                (MOON_ORE_ZEOLITES, 4_500_u64, 10.0_f64),
                (MOON_ORE_COBALTITE, 3_500_u64, 10.0_f64),
            ];

            let mut spawned_ids = Vec::new();
            let base_pos = DVec3::new(st_pos[0], st_pos[1], st_pos[2]);

            for (idx, &(ore_tid, units, uvol)) in ores_to_spawn.iter().enumerate() {
                let ast_id = 950_000_000 + structure_id % 100_000 * 100 + (idx as i64) + 1;
                let offset = DVec3::new(
                    (idx as f64 + 1.0) * 4_000.0,
                    (idx as f64) * 2_500.0,
                    -(idx as f64 + 1.0) * 3_500.0,
                );
                let ball = Ball {
                    id: ast_id,
                    type_id: ore_tid,
                    position: base_pos + offset,
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
                spawned_ids.push(ast_id);
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(ext) = snap.moon_extractions.get_mut(&structure_id) else {
                    return Err(BridgeRpcError {
                        code: "NoActiveExtraction".into(),
                        message: format!("No active extraction to fracture on structure {structure_id}"),
                    });
                };
                ext.status = "Fractured".into();
                ext.spawned_asteroid_ids = spawned_ids.clone();
                Ok(spawned_ids.clone())
            });

            match res {
                Ok(ast_ids) => {
                    let ore_tids: Vec<PyRep> = ores_to_spawn
                        .iter()
                        .map(|&(tid, _, _)| PyRep::Integer(tid as i64))
                        .collect();
                    Some(Ok(PyRep::keyval([
                        ("structureID", PyRep::Integer(structure_id)),
                        ("fractured", PyRep::Bool(true)),
                        ("status", PyRep::String("Fractured".into())),
                        ("asteroidCount", PyRep::Integer(ast_ids.len() as i64)),
                        ("oreTypes", PyRep::List(ore_tids)),
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
                    PyRep::keyval([
                        ("structureID", PyRep::Integer(e.structure_id)),
                        ("moonID", PyRep::Integer(e.moon_id)),
                        ("extractionStartTime", PyRep::Integer(e.extraction_start_unix as i64)),
                        ("chunkArrivalTime", PyRep::Integer(e.chunk_arrival_unix as i64)),
                        ("naturalDecayTime", PyRep::Integer(e.natural_decay_unix as i64)),
                        ("status", PyRep::String(e.status.clone())),
                        ("readyToFracture", PyRep::Bool(e.status == "Ready" || e.status == "Fractured")),
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

            let moon_id = ctx.db.with_snapshot(|snap| {
                snap.moon_extractions
                    .get(&structure_id)
                    .map(|e| e.moon_id)
                    .unwrap_or_else(|| 40_009_087)
            });

            let resources = vec![
                (MOON_ORE_BITUMENS, "Bitumens", 0.40),
                (MOON_ORE_ZEOLITES, "Zeolites", 0.35),
                (MOON_ORE_COBALTITE, "Cobaltite", 0.25),
            ];

            let list: Vec<PyRep> = resources
                .into_iter()
                .map(|(tid, name, frac)| {
                    PyRep::keyval([
                        ("typeID", PyRep::Integer(tid as i64)),
                        ("oreName", PyRep::String(name.to_string())),
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

            let ext = ctx.db.with_snapshot(|snap| {
                snap.moon_extractions.get(&structure_id).cloned()
            });

            if let Some(e) = ext {
                Some(Ok(PyRep::keyval([
                    ("structureID", PyRep::Integer(e.structure_id)),
                    ("moonID", PyRep::Integer(e.moon_id)),
                    ("extractionStartTime", PyRep::Integer(e.extraction_start_unix as i64)),
                    ("chunkArrivalTime", PyRep::Integer(e.chunk_arrival_unix as i64)),
                    ("naturalDecayTime", PyRep::Integer(e.natural_decay_unix as i64)),
                    ("status", PyRep::String(e.status)),
                    ("eventID", PyRep::Integer(e.event_id)),
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
                        message: "Character lacks corporation permissions for auto moon mining".into(),
                    });
                }

                let auto_miner = snap.auto_moon_miners.entry(structure_id).or_insert_with(|| {
                    AutoMoonMinerRecord {
                        structure_id,
                        fuel_units_remaining: 100,
                        magmatic_gas_remaining: 200,
                        efficiency: 0.40, // 40% Metenox Moon Drill baseline
                        cycle_duration_seconds: 3600,
                        last_cycle_unix: ctx.now_unix(),
                        ore_type_id: MOON_ORE_BITUMENS,
                        hourly_yield: 500,
                        status: "Active".into(),
                    }
                });

                Ok(auto_miner.clone())
            });

            match res {
                Ok(rec) => Some(Ok(PyRep::keyval([
                    ("structureID", PyRep::Integer(rec.structure_id)),
                    ("efficiency", PyRep::Real(rec.efficiency)),
                    ("cycleTimeSeconds", PyRep::Integer(rec.cycle_duration_seconds as i64)),
                    ("fuelConsumptionPerCycle", PyRep::Integer(5)),
                    ("magmaticGasPerCycle", PyRep::Integer(10)),
                    ("fuelRemaining", PyRep::Integer(rec.fuel_units_remaining as i64)),
                    ("magmaticGasRemaining", PyRep::Integer(rec.magmatic_gas_remaining as i64)),
                    ("status", PyRep::String(rec.status)),
                    ("hourlyYield", PyRep::Integer(rec.hourly_yield as i64)),
                    ("oreTypeID", PyRep::Integer(rec.ore_type_id as i64)),
                ]))),
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

            let cycles = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1).max(1) as u32;

            let now = ctx.now_unix();
            let date = eve_db::unix_secs_to_date_string(now);

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
                        message: "Character lacks corporation permissions for auto moon mining".into(),
                    });
                }

                let auto_miner = snap.auto_moon_miners.entry(structure_id).or_insert_with(|| {
                    AutoMoonMinerRecord {
                        structure_id,
                        fuel_units_remaining: 100,
                        magmatic_gas_remaining: 200,
                        efficiency: 0.40,
                        cycle_duration_seconds: 3600,
                        last_cycle_unix: now,
                        ore_type_id: MOON_ORE_BITUMENS,
                        hourly_yield: 500,
                        status: "Active".into(),
                    }
                });

                let fuel_req = cycles * 5;
                let gas_req = cycles * 10;

                if auto_miner.fuel_units_remaining < fuel_req {
                    auto_miner.status = "OutOfFuel".into();
                    return Err(BridgeRpcError {
                        code: "OutOfFuel".into(),
                        message: format!(
                            "Auto moon drill has {} fuel blocks, but {} required for {cycles} cycles",
                            auto_miner.fuel_units_remaining, fuel_req
                        ),
                    });
                }

                auto_miner.fuel_units_remaining -= fuel_req;
                auto_miner.magmatic_gas_remaining = auto_miner.magmatic_gas_remaining.saturating_sub(gas_req);
                auto_miner.last_cycle_unix = now;
                auto_miner.status = "Active".into();

                let units_produced = ((cycles as f64) * (auto_miner.hourly_yield as f64) * auto_miner.efficiency).round() as u64;
                let ore_tid = auto_miner.ore_type_id;

                // Deliver mined moon ore to structure inventory / hangar
                snap.next_entity_id += 1;
                let item_id = snap.next_entity_id;
                snap.items.insert(
                    item_id,
                    InventoryItemRecord {
                        item_id,
                        type_id: ore_tid,
                        owner_id: st.owner_corp_id,
                        location_id: structure_id,
                        flag: InventoryFlag::Hangar,
                        quantity: units_produced as i32,
                        singleton: false,
                        version: 1,
                        ..Default::default()
                    },
                );

                // Record automated extraction in observer mining ledger
                snap.observer_mining_ledger
                    .entry(structure_id)
                    .or_default()
                    .push(ObserverMiningRecord {
                        observer_id: structure_id,
                        character_id: char_id,
                        recorded_corporation_id: caller_corp,
                        type_id: ore_tid,
                        quantity: units_produced as i64,
                        last_updated_date: date.clone(),
                        timestamp_unix: now,
                    });

                Ok((
                    structure_id,
                    cycles,
                    units_produced,
                    ore_tid,
                    fuel_req,
                    gas_req,
                    auto_miner.fuel_units_remaining,
                ))
            });

            match res {
                Ok((s_id, cyc, produced, ore_tid, f_used, g_used, f_rem)) => {
                    Some(Ok(PyRep::keyval([
                        ("structureID", PyRep::Integer(s_id)),
                        ("cyclesProcessed", PyRep::Integer(cyc as i64)),
                        ("unitsProduced", PyRep::Integer(produced as i64)),
                        ("oreTypeID", PyRep::Integer(ore_tid as i64)),
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

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut remainder_record = None;
                let mut compressed_out = None;

                if let Some(item) = snap.items.get_mut(&item_id) {
                    if item.owner_id == char_id
                        && (Some(item.location_id) == ship_id)
                        && item.flag != InventoryFlag::Locked
                        && item.quantity >= 100
                    {
                        if let Some(&comp_type) = ctx.sde.compressible_types.get(&item.type_id) {
                            let orig_type = item.type_id;
                            let orig_loc = item.location_id;
                            let orig_flag = item.flag;
                            let rem = item.quantity % 100;
                            item.type_id = comp_type;
                            item.quantity /= 100;
                            item.version += 1;
                            compressed_out = Some((item.item_id, item.type_id, item.quantity, rem));
                            if rem > 0 {
                                remainder_record = Some((orig_type, orig_loc, orig_flag, rem));
                            }
                        }
                    }
                }

                if let Some((orig_type, orig_loc, orig_flag, rem)) = remainder_record {
                    snap.next_entity_id += 1;
                    let rem_id = snap.next_entity_id;
                    snap.items.insert(
                        rem_id,
                        InventoryItemRecord {
                            item_id: rem_id,
                            type_id: orig_type,
                            owner_id: char_id,
                            location_id: orig_loc,
                            flag: orig_flag,
                            quantity: rem,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        },
                    );
                }

                compressed_out.ok_or_else(|| BridgeRpcError {
                    code: "CompressionFailed".into(),
                    message: "Item cannot be compressed in space (must be >=100 units of compressible type in ship hold)".into(),
                })
            });

            match res {
                Ok((iid, ctype, qty, rem)) => Some(Ok(PyRep::keyval([
                    ("itemID", PyRep::Integer(iid)),
                    ("compressedTypeID", PyRep::Integer(ctype as i64)),
                    ("quantity", PyRep::Integer(qty as i64)),
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
                    message: "CompressItemInStructure requires being docked at a station or structure".into(),
                }));
            }

            // Args can be [structure_id, item_id] or [item_id]
            let item_id = if args.len() >= 2 {
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
                let mut remainder_record = None;
                let mut compressed_out = None;

                if let Some(item) = snap.items.get_mut(&item_id) {
                    let reachable = (Some(item.location_id) == session.station_id)
                        || (Some(item.location_id) == session.structure_id)
                        || (Some(item.location_id) == session.ship_id);
                    let portion_size = ctx
                        .sde
                        .types
                        .get(&item.type_id)
                        .map(|t| t.portion_size as i32)
                        .unwrap_or(100)
                        .max(1);
                    if item.owner_id == char_id
                        && reachable
                        && item.flag != InventoryFlag::Locked
                        && item.quantity >= portion_size
                    {
                        if let Some(&comp_type) = ctx.sde.compressible_types.get(&item.type_id) {
                            let orig_type = item.type_id;
                            let orig_loc = item.location_id;
                            let orig_flag = item.flag;
                            let rem = item.quantity % portion_size;
                            item.type_id = comp_type;
                            item.quantity /= portion_size;
                            item.version += 1;
                            compressed_out = Some((item.item_id, item.type_id, item.quantity, rem));
                            if rem > 0 {
                                remainder_record = Some((orig_type, orig_loc, orig_flag, rem));
                            }
                        }
                    }
                }

                if let Some((orig_type, orig_loc, orig_flag, rem)) = remainder_record {
                    snap.next_entity_id += 1;
                    let rem_id = snap.next_entity_id;
                    snap.items.insert(
                        rem_id,
                        InventoryItemRecord {
                            item_id: rem_id,
                            type_id: orig_type,
                            owner_id: char_id,
                            location_id: orig_loc,
                            flag: orig_flag,
                            quantity: rem,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        },
                    );
                }

                compressed_out.ok_or_else(|| BridgeRpcError {
                    code: "CompressionFailed".into(),
                    message: "Item cannot be compressed (invalid type, <100 units, locked, or unreachable location)".into(),
                })
            });

            match res {
                Ok((iid, ctype, qty, rem)) => Some(Ok(PyRep::keyval([
                    ("itemID", PyRep::Integer(iid)),
                    ("compressedTypeID", PyRep::Integer(ctype as i64)),
                    ("quantity", PyRep::Integer(qty as i64)),
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
                    message: "DecompressGasInStructure requires being docked at a station or structure".into(),
                }));
            }

            let item_id = if args.len() >= 2 {
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

            let efficiency = 0.90_f64; // 90% gas decompression efficiency baseline

            let res = ctx.db.try_with_mut_snapshot(|snap| {
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

                    let total_raw = (item.quantity as f64 * 100.0 * efficiency).round() as i64;
                    // Consume the compressed gas item
                    item.quantity = 0;
                    (raw_tid, total_raw)
                };

                // Remove 0-qty item
                snap.items.remove(&item_id);

                // Add decompressed gas item to hangar
                let dock_loc = session.station_id.or(session.structure_id).unwrap_or(0);
                snap.next_entity_id += 1;
                let new_id = snap.next_entity_id;
                snap.items.insert(
                    new_id,
                    InventoryItemRecord {
                        item_id: new_id,
                        type_id: raw_tid,
                        owner_id: char_id,
                        location_id: dock_loc,
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
                    ("efficiency", PyRep::Real(efficiency)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("structureCompressionMgr", "GetMyGasDecompressionEfficiency") => {
            Some(Ok(PyRep::Real(0.90)))
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

            // Update npe progress with verified ledger count (rejecting any client-supplied counts)
            let _ = ctx.db.try_with_mut_snapshot(|snap| {
                let npe = snap.npe_progress.entry(char_id).or_insert_with(|| {
                    eve_db::NpeProgressRecord {
                        character_id: char_id,
                        node_graph_id: 473,
                        current_node_id: 1,
                        completed: false,
                        career_goal_progress: Default::default(),
                        claimed_goals: Default::default(),
                    }
                });
                npe.career_goal_progress.insert(goal_id, total_mined);
                Ok::<(), std::convert::Infallible>(())
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
