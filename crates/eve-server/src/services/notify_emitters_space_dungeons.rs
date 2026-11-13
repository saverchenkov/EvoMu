//! `notify_emitters_space_dungeons` — Server-side V24 notification emitters for Space & Dungeons
//! (Space, Destiny, ESS, PvE Dungeons/Escalations, Godma/EWAR, Drones, Crimewatch, Structures, PvP/Tourney).

#![forbid(unsafe_code)]

use crate::router::notify_registry::*;
use crate::router::{NotificationTarget, ServerContext};
use eve_proto::{DBRowDescriptor, PyRep};

/// Builds the V24 `OnDamageStateChange` 3-tuple (`#990`):
/// `((shieldRatio, shieldTauMs, timestampFt), armorRatio, structureRatio)`,
/// where `shieldTauMs = recharge_ms / 5.0` and `timestampFt` uses real OS wallclock FILETIME ticks
/// matching live client `blue.os.GetSimTime()`.
pub fn build_damage_state_tuple(
    ctx: &ServerContext,
    item_id: i64,
    shield: f64,
    armor: f64,
    structure: f64,
) -> PyRep {
    let (secs, nanos) = eve_db::clock::host_wall_unix_secs_and_nanos();
    let sim_ft = (secs as i64 + 11_644_473_600) * 10_000_000 + (nanos as i64 / 100);
    let recharge_ms = ctx
        .db
        .with_snapshot(|snap| {
            let it = snap.items.get(&item_id)?;
            it.dynamic_attributes
                .get(&479)
                .copied()
                .or_else(|| {
                    ctx.sde
                        .types
                        .get(&it.type_id)
                        .and_then(|t| t.default_attributes.get(&479).copied())
                })
        })
        .filter(|&v| v > 0.0)
        .unwrap_or(100_000.0);
    let tau_ms = (recharge_ms / 5.0).max(1.0);
    PyRep::Tuple(vec![
        PyRep::Tuple(vec![
            PyRep::Real(shield.clamp(0.0, 1.0)),
            PyRep::Real(tau_ms),
            PyRep::Integer(sim_ft),
        ]),
        PyRep::Real(armor.clamp(0.0, 1.0)),
        PyRep::Real(structure.clamp(0.0, 1.0)),
    ])
}

/// Builds a V24 `carbon.common.script.sys.row.Row` (`util.Row`) marshaled via `Op_PyObjectEx2`.
pub fn build_util_row(columns: &[&str], values: Vec<PyRep>) -> PyRep {
    PyRep::Object {
        type_name: "util.Row".into(),
        args: Box::new(PyRep::Dict(vec![
            (
                PyRep::String("header".into()),
                PyRep::List(columns.iter().map(|&c| PyRep::String(c.into())).collect()),
            ),
            (PyRep::String("line".into()), PyRep::List(values)),
        ])),
    }
}

/// Emits `OnCaptureChanged(ballID, captureID, lastIncident, points, captureTime, lastCapturing)`
/// (`6` args, `id_type = "solarsystemid"`) upon FW complex capture progress/completion.
pub fn emit_fw_capture_changed(
    ctx: &ServerContext,
    solar_system_id: u32,
    ball_id: i64,
    faction_id: i64,
    points: f64,
    capture_time_ms: f64,
) {
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.emit_typed(
        NotificationTarget::SolarSystem(solar_system_id),
        OnCaptureChanged {
            ball_id,
            capture_id: faction_id,
            last_incident: now_ft,
            points,
            capture_time: capture_time_ms,
            last_capturing: faction_id,
        },
    );
}

/// Emits `OnCfgDataChanged("evelocations", [structureID, name, solarSystemID, x, y, z, None])`
/// and, for Tenebrex Cyno Jammer (`type_id == 37534`), `OnCynoJammerChanged(solarsystemID, onlineSimTime)`.
pub fn emit_structure_cfg_and_jammer_notifications(
    ctx: &ServerContext,
    sys_id: u32,
    structure_id: i64,
    type_id: u32,
    name: &str,
    pos: [f64; 3],
    online: bool,
) {
    ctx.emit_typed(
        NotificationTarget::SolarSystem(sys_id),
        OnCfgDataChanged {
            what: "evelocations".into(),
            data: PyRep::List(vec![
                PyRep::Integer(structure_id),
                PyRep::String(name.to_string()),
                PyRep::Integer(i64::from(sys_id)),
                PyRep::Real(pos[0]),
                PyRep::Real(pos[1]),
                PyRep::Real(pos[2]),
                PyRep::None,
            ]),
        },
    );
    if type_id == 37_534 {
        let sim_time = if online {
            Some(ctx.now_blueos_filetime_ticks())
        } else {
            None
        };
        ctx.emit_typed(
            NotificationTarget::SolarSystem(sys_id),
            OnCynoJammerChangedOpt {
                solar_system_id: i64::from(sys_id),
                status: sim_time,
            },
        );
    }
}

/// Emits `OnESSMainBankLinkNotification`, `OnESSReserveBankLinkNotification`, or
/// `OnMainBankPlayerHackSuccess` according to the ESS bank link outcome.
pub fn emit_ess_bank_link_notifications(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    method: &str,
    link_started_only: bool,
    payout: f64,
) {
    if method == "AttemptLinkToMainBank" {
        if link_started_only {
            ctx.emit_typed(
                NotificationTarget::SolarSystem(sys_id),
                OnESSMainBankLinkNotification {
                    solar_system_id: i64::from(sys_id),
                },
            );
        } else if payout > 0.0 {
            ctx.emit_typed_with_id_type(
                NotificationTarget::SolarSystem(sys_id),
                "solarsystemid",
                OnMainBankPlayerHackSuccess {
                    data: PyRep::Dict(vec![
                        (
                            PyRep::String("solarSystemID".into()),
                            PyRep::Integer(i64::from(sys_id)),
                        ),
                        (PyRep::String("linkID".into()), PyRep::Integer(1)),
                        (PyRep::String("characterID".into()), PyRep::Integer(char_id)),
                        (PyRep::String("amount".into()), PyRep::Real(payout)),
                    ]),
                },
            );
        }
    } else if method == "AttemptLinkToReserveBank" && link_started_only {
        ctx.emit_typed(
            NotificationTarget::SolarSystem(sys_id),
            OnESSReserveBankLinkNotification {
                solar_system_id: i64::from(sys_id),
            },
        );
    }
}

/// Emits `OnMainBankPlayerLinkDisconnected(data)` when an ESS main bank link is severed.
pub fn emit_ess_main_bank_link_disconnected(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    reason: &str,
) {
    ctx.emit_typed_with_id_type(
        NotificationTarget::SolarSystem(sys_id),
        "solarsystemid",
        OnMainBankPlayerLinkDisconnected {
            data: PyRep::Dict(vec![
                (
                    PyRep::String("solarSystemID".into()),
                    PyRep::Integer(i64::from(sys_id)),
                ),
                (PyRep::String("linkID".into()), PyRep::Integer(1)),
                (PyRep::String("characterID".into()), PyRep::Integer(char_id)),
                (
                    PyRep::String("reason".into()),
                    PyRep::String(reason.to_string()),
                ),
            ]),
        },
    );
}

/// Spawns a dynamic cosmic anomaly in `pve_exploration` and emits `OnCosmicAnomalyAdded(anomaly)`.
pub fn spawn_and_emit_cosmic_anomaly(
    ctx: &ServerContext,
    sys_id: u32,
    dungeon_id: u32,
) -> (String, i64) {
    let now_unix = ctx.now_unix();
    let mut out_anom_id = String::new();
    let mut out_inst_id = 0i64;
    let mut out_name = String::from("Combat Anomaly");
    crate::services::pve_exploration::with_pve_exploration_reg(ctx, |reg| {
        let seq = (reg.sites_by_system.get(&sys_id).map_or(0, Vec::len) + 1) as u32;
        out_inst_id = 910_000_000 + i64::from(seq);
        out_anom_id = format!("ANM-{sys_id}-{seq:03}");
        out_name = format!("Dynamic Combat Anomaly #{seq}");
        let site = crate::services::pve_exploration::PveSiteInstance {
            site_id: out_anom_id.clone(),
            dungeon_id,
            dungeon_name: out_name.clone(),
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            position: [175_000.0, 25_000.0, -40_000.0],
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: 2,
            created_unix: now_unix,
            lifetime_seconds: 86_400,
            ..Default::default()
        };
        reg.sites_by_system.entry(sys_id).or_default().push(site);
    });
    ctx.db.with_mut_snapshot(|snap| {
        let list = snap.cosmic_anomalies.entry(sys_id).or_default();
        if !list.iter().any(|a| a.anomaly_id == out_anom_id) {
            list.push(eve_db::CosmicAnomalyRecord {
                anomaly_id: out_anom_id.clone(),
                solar_system_id: sys_id,
                name: out_name.clone(),
                anomaly_type: "Combat".into(),
                position: [175_000.0, 25_000.0, -40_000.0],
                signal_strength: 1.0,
                ore_types: Vec::new(),
                remaining_volume: 0.0,
                created_unix: now_unix,
                lifetime_seconds: 86_400,
                cleared: false,
            });
        }
    });
    ctx.emit_typed(
        NotificationTarget::SolarSystem(sys_id),
        OnCosmicAnomalyAdded {
            anomaly: PyRep::keyval([
                ("targetID", PyRep::String(out_anom_id.clone())),
                ("instanceID", PyRep::Integer(out_inst_id)),
                ("dungeonID", PyRep::Integer(i64::from(dungeon_id))),
                ("dungeonName", PyRep::String(out_name)),
                ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
            ]),
        },
    );
    (out_anom_id, out_inst_id)
}

/// Registers a cosmic signature in `snap.cosmic_signatures` and emits `OnCosmicSignatureAdded(target_id)`.
pub fn emit_cosmic_signature_added(
    ctx: &ServerContext,
    sys_id: u32,
    sig_id: &str,
    name: &str,
    pos: [f64; 3],
) {
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let list = snap.cosmic_signatures.entry(sys_id).or_default();
        if !list.iter().any(|s| s.signature_id == sig_id) {
            list.push(eve_db::CosmicSignatureRecord {
                signature_id: sig_id.to_string(),
                solar_system_id: sys_id,
                sig_group: "Combat".into(),
                type_name: name.to_string(),
                position: pos,
                signal_strength: 1.0,
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: now_unix,
                lifetime_seconds: 86_400,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            });
        }
    });
    ctx.emit_typed(
        NotificationTarget::SolarSystem(sys_id),
        OnCosmicSignatureAdded {
            target_id: sig_id.to_string(),
        },
    );
}

/// Emits escalation journal notifications (`OnEscalatingPathChange`, 1-arg `OnEscalatingPathMessage(instanceID)`,
/// and `OnFleetEscalatingPathMessage(charID, templateID)` to fleetmates when in a fleet).
pub fn emit_escalation_created_or_advanced(
    ctx: &ServerContext,
    char_id: i64,
    instance_id: i64,
    dungeon_template_id: u32,
    notify_fleet: bool,
) {
    ctx.emit_typed(NotificationTarget::Character(char_id), OnEscalatingPathChange);
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnEscalatingPathMessage { instance_id },
    );
    if notify_fleet {
        let fleet_id_opt = ctx
            .db
            .with_snapshot(|s| s.characters.get(&char_id).and_then(|c| c.fleet_id));
        if let Some(fid) = fleet_id_opt.filter(|&f| f > 0) {
            ctx.emit_typed(
                NotificationTarget::Fleet(fid),
                OnFleetEscalatingPathMessage {
                    char_id,
                    journal_entry_template_id: i64::from(dungeon_template_id),
                },
            );
        }
    }
}

/// Emits dungeon room/wave progression notifications (`OnEnteringDungeonRoom`, `OnDungeonValues`,
/// `OnEscalatingPathDungeonEntered` or `OnDistributionDungeonEntered`, and `OnDungeonCompleted`).
#[allow(clippy::too_many_arguments)]
pub fn emit_dungeon_progression_notifications(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    category: &str,
    archetype: &str,
    dungeon_id: i64,
    instance_id: i64,
    room_id: i64,
    room_pos: [f64; 3],
    current_wave: u8,
    total_waves: u8,
    completed_now: bool,
    expiry_ft: i64,
) {
    let values_dict = PyRep::Dict(vec![
        (
            PyRep::String("currentWave".into()),
            PyRep::Integer(i64::from(current_wave)),
        ),
        (
            PyRep::String("totalWaves".into()),
            PyRep::Integer(i64::from(total_waves)),
        ),
        (
            PyRep::String("completed".into()),
            PyRep::Bool(completed_now),
        ),
    ]);
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnEnteringDungeonRoom {
            dungeon_id,
            room_id,
            room_position: PyRep::Tuple(vec![
                PyRep::Real(room_pos[0]),
                PyRep::Real(room_pos[1]),
                PyRep::Real(room_pos[2]),
            ]),
            instance_id,
            dungeon_values: values_dict.clone(),
        },
    );
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnDungeonValues {
            dungeon_id,
            values: values_dict,
        },
    );
    let room_objective_id = room_id * 10 + i64::from(current_wave.max(1));
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnProgressionRoomJoined {
            progression_name_id: dungeon_id,
            objective_id: room_objective_id,
            task_widget_states: PyRep::List(Vec::new()),
            objective_history: Vec::new(),
            room_id,
        },
    );

    let row = build_util_row(
        &[
            "instanceID",
            "dungeonID",
            "dungeonName",
            "dungeonNameID",
            "solarSystemID",
            "expiryTime",
        ],
        vec![
            PyRep::Integer(instance_id),
            PyRep::Integer(dungeon_id),
            PyRep::String(archetype.to_string()),
            PyRep::None,
            PyRep::Integer(i64::from(sys_id)),
            PyRep::Integer(expiry_ft),
        ],
    );
    if matches!(category, "Expedition" | "DED" | "CombatSignature") {
        ctx.emit_typed(
            NotificationTarget::Character(char_id),
            OnEscalatingPathDungeonEntered { row },
        );
    } else {
        ctx.emit_typed(
            NotificationTarget::Character(char_id),
            OnDistributionDungeonEntered { row },
        );
    }

    if completed_now {
        ctx.emit_typed(
            NotificationTarget::Character(char_id),
            OnDungeonCompleted {
                dungeon_id: i64::from(dungeon_id),
            },
        );
    }
}

/// Emits `OnExitingDungeon(dungeonID)` if `ship_pos` is departing an active PvE dungeon pocket.
pub fn emit_exiting_dungeon_on_warp_if_present(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    ship_pos: [f64; 3],
) {
    let found_did =
        crate::services::pve_dungeon_runtime::find_active_dungeon_id_at_position(ctx, sys_id, ship_pos)
            .or_else(|| {
                crate::services::pve_exploration::with_pve_exploration_reg(ctx, |reg| {
                    reg.sites_by_system.get(&sys_id).and_then(|sites| {
                        sites
                            .iter()
                            .find(|s| {
                                let dx = s.position[0] - ship_pos[0];
                                let dy = s.position[1] - ship_pos[1];
                                let dz = s.position[2] - ship_pos[2];
                                (dx * dx + dy * dy + dz * dz).sqrt() <= 150_000.0
                            })
                            .map(|s| s.dungeon_id)
                    })
                })
            });
    if let Some(dungeon_id) = found_did {
        ctx.emit_typed(
            NotificationTarget::Character(char_id),
            OnExitingDungeon {
                dungeon_id: i64::from(dungeon_id),
            },
        );
    }
}

/// Emits `OnDungeonEdit(dungeonID, roomID, roomPos)` on GM dungeon editor room load/edit.
pub fn emit_dungeon_edit(
    ctx: &ServerContext,
    char_id: i64,
    dungeon_id: i64,
    room_id: i64,
    room_pos: [f64; 3],
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnDungeonEdit {
            dungeon_id,
            room_id,
            room_pos: PyRep::Tuple(vec![
                PyRep::Real(room_pos[0]),
                PyRep::Real(room_pos[1]),
                PyRep::Real(room_pos[2]),
            ]),
        },
    );
}

/// Emits `OnDroneActivityChange(droneID, activityID, activity)` to the controlling pilot.
pub fn emit_drone_activity_change(
    ctx: &ServerContext,
    char_id: i64,
    drone_id: i64,
    activity_id: i64,
    activity: Option<&str>,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnDroneActivityChange {
            drone_id,
            activity_id,
            activity: activity.map(str::to_string),
        },
    );
}

/// Emits `OnGodmaEffectInfo` and `OnGodmaPrimeItem` when a module is brought online
/// (or for synthetic non-inventory test modules). Already-primed fitted modules in
/// `ctx.db.items` must NOT receive `OnGodmaEffectInfo` or `OnGodmaPrimeItem` on
/// space activation (`effect_name != "online"`), because `StateManager.RefreshItemEffects`
/// replaces the `blue.DBRow` entries in `effectsByItemEffect[itemID]` and orphans
/// `ModuleButton.def_effect.dbrow` (`#975`).
#[allow(clippy::too_many_arguments)]
pub fn emit_module_activation_godma_notifications(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    module_type_id: u32,
    effect_name: &str,
    duration_s: f64,
    target_id_opt: Option<i64>,
) {
    if effect_name != "online" && ctx.db.get_item(module_id).is_some() {
        return;
    }
    let Some(effect_id) = crate::router::binding::effect_id_for_module_sde(
        &ctx.sde,
        module_type_id,
        Some(effect_name),
    ) else {
        return;
    };
    let now_ft = ctx.now_client_sim_filetime_ticks();
    let dur_ms_f64 = (duration_s.max(1.0)) * 1_000.0;
    let dur_ms = dur_ms_f64.round() as i64;
    let active_effect_line = PyRep::List(vec![
        PyRep::Integer(module_id),
        PyRep::Integer(char_id),
        PyRep::Integer(ship_id),
        target_id_opt.map(PyRep::Integer).unwrap_or(PyRep::None),
        PyRep::None,
        PyRep::List(Vec::new()),
        PyRep::Integer(effect_id),
        PyRep::Integer(now_ft),
        PyRep::Integer(dur_ms.max(0)),
        PyRep::Integer(0),
    ]);
    let active_effects = PyRep::Dict(vec![(PyRep::Integer(effect_id), active_effect_line)]);
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnGodmaEffectInfo {
            item_id: module_id,
            type_id: i64::from(module_type_id),
            active_effects: active_effects.clone(),
            time: now_ft,
        },
    );

    if let Some(mut inv_item) = ctx.db.get_item(module_id) {
        inv_item.singleton = true;
        let inv_row = DBRowDescriptor::inv_item_header()
            .build_dbrow(crate::services::inventory_fitting::item_to_packed_cells(ctx, &inv_item));
        let mut merged_attrs: std::collections::BTreeMap<u16, f64> = ctx
            .sde
            .types
            .get(&module_type_id)
            .map(|t| t.default_attributes.iter().map(|(&k, &v)| (k, v)).collect())
            .unwrap_or_default();
        for (&k, &v) in &inv_item.dynamic_attributes {
            merged_attrs.insert(k, v);
        }
        if ship_id > 0 {
            let owner_cid = if char_id > 0 { char_id } else { inv_item.owner_id };
            if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(owner_cid, ship_id) {
                if let Some(m_state) = fit.module_state(module_id, &ctx.sde) {
                    let modified_targets: std::collections::HashSet<u16> = m_state
                        .modifiers
                        .iter()
                        .map(|m| m.target_attribute_id)
                        .collect();
                    let all_aids: std::collections::BTreeSet<u16> = m_state
                        .base_attributes
                        .keys()
                        .copied()
                        .chain(modified_targets.iter().copied())
                        .collect();
                    for aid in all_aids {
                        if inv_item.dynamic_attributes.contains_key(&aid)
                            && !modified_targets.contains(&aid)
                        {
                            continue;
                        }
                        merged_attrs.insert(aid, m_state.get_attribute(aid, &ctx.sde));
                    }
                    let mut rtc_attrs: std::collections::HashMap<u16, f64> =
                        merged_attrs.iter().map(|(&k, &v)| (k, v)).collect();
                    crate::services::tactical_modules::apply_projected_rtc_to_module_attrs(
                        ctx,
                        ship_id,
                        &mut rtc_attrs,
                    );
                    merged_attrs = rtc_attrs.into_iter().collect();
                }
            }
        }
        merged_attrs.entry(73).or_insert(dur_ms_f64);
        merged_attrs.insert(2, 1.0);
        let attrs_dict = PyRep::Dict(
            merged_attrs
                .into_iter()
                .map(|(k, v)| (PyRep::Integer(i64::from(k)), PyRep::Real(v)))
                .collect(),
        );
        let prime_row = PyRep::keyval([
            ("itemID", PyRep::Integer(module_id)),
            ("invItem", inv_row),
            ("attributes", attrs_dict),
            ("activeEffects", active_effects),
            ("time", PyRep::Integer(now_ft)),
            ("wallclockTime", PyRep::Integer(now_ft)),
        ]);
        ctx.emit_typed(
            NotificationTarget::Character(char_id),
            OnGodmaPrimeItem {
                location_id: ship_id,
                row: prime_row,
            },
        );
    }
}

/// Removes active/projected EWAR state for `(ship_id, module_id)` and emits
/// `OnEwarEnd(sourceBallID, moduleID, targetBallID, jammingType)` without purging
/// a still-fitted module from `godma.StateManager.invitems`.
pub fn flush_module_ewar_end(
    ctx: &ServerContext,
    char_id: i64,
    solar_system_id: Option<u32>,
    ship_id: i64,
    module_id: i64,
) -> Vec<i64> {
    let started_targets = ctx
        .active_ewar_starts
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(ship_id, module_id))
        .unwrap_or_default();
    let ewar_targets: Vec<(i64, &'static str)> = if !started_targets.is_empty() {
        started_targets
    } else {
        let cached_source = ctx
            .projected_ewar_sources
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&module_id)
            .copied();
        let source_matches = cached_source
            .map(|sid| sid == ship_id)
            .unwrap_or_else(|| {
                ctx.db
                    .with_snapshot(|s| s.items.get(&module_id).is_some_and(|i| i.location_id == ship_id))
            });
        let projected_targets: Vec<(i64, u32, Option<u32>, bool)> = if source_matches {
            let guard = ctx
                .projected_ewar
                .read()
                .unwrap_or_else(|e| e.into_inner());
            let mut out = Vec::new();
            for (&target_sid, list) in guard.iter() {
                for eff in list {
                    if eff.source_module_id == module_id {
                        let disables_mwd = ctx
                            .sde
                            .types
                            .get(&eff.source_module_type_id)
                            .is_some_and(|t| t.group_id == 52)
                            && eve_dogma::eval_warp_scrambler_effects(
                                &[&eff.effective_module],
                                &[0.0],
                                &ctx.sde,
                            )
                            .blocks_mwd;
                        out.push((
                            target_sid,
                            eff.source_module_type_id,
                            eff.charge_type_id,
                            disables_mwd,
                        ));
                    }
                }
            }
            out
        } else {
            Vec::new()
        };
        projected_targets
            .into_iter()
            .map(|(target_sid, mod_tid, charge_tid, disables_mwd)| {
                let jamming_type = ctx
                    .sde
                    .types
                    .get(&mod_tid)
                    .and_then(|mod_type| {
                        crate::services::inventory_fitting::fitting_ewar_capital::ewar_jamming_type(
                            mod_type,
                            charge_tid,
                            disables_mwd,
                            None,
                        )
                    })
                    .unwrap_or("electronic");
                (target_sid, jamming_type)
            })
            .collect()
    };
    let affected_projected = ctx.remove_projected_ewar_by_module(ship_id, module_id);
    for (target_sid, jamming_type) in ewar_targets {
        let mut notified = false;
        if char_id > 0 {
            ctx.emit_typed(
                NotificationTarget::Character(char_id),
                OnEwarEnd {
                    source_ball_id: ship_id,
                    module_id,
                    target_ball_id: target_sid,
                    jamming_type: jamming_type.into(),
                },
            );
            notified = true;
        }
        let target_owner = ctx
            .db
            .with_snapshot(|snap| snap.items.get(&target_sid).map(|i| i.owner_id))
            .unwrap_or(0);
        if target_owner > 0 && target_owner != char_id {
            ctx.emit_typed(
                NotificationTarget::Character(target_owner),
                OnEwarEnd {
                    source_ball_id: ship_id,
                    module_id,
                    target_ball_id: target_sid,
                    jamming_type: jamming_type.into(),
                },
            );
            notified = true;
        }
        if !notified {
            if let Some(sys_id) = solar_system_id {
                ctx.emit_typed_with_id_type(
                    NotificationTarget::SolarSystem(sys_id),
                    "solarsystemid",
                    OnEwarEnd {
                        source_ball_id: ship_id,
                        module_id,
                        target_ball_id: target_sid,
                        jamming_type: jamming_type.into(),
                    },
                );
            }
        }
    }
    affected_projected
}

/// Emits `OnGodmaFlushLocationProfile(shipID, ("effect", moduleID))` (only safe when
/// `module_id` is actually leaving `ship_id`, because client `StateManager.PurgeEntry`
/// deletes `module_id` from `godma.invitems`) and cleans up any projected EWAR.
// parity: notify:OnGodmaFlushLocationProfile
pub fn flush_module_profile_and_ewar_end(
    ctx: &ServerContext,
    char_id: i64,
    solar_system_id: Option<u32>,
    ship_id: i64,
    module_id: i64,
) -> Vec<i64> {
    if ship_id > 0 && module_id > 0 && module_id != ship_id {
        ctx.emit_typed(
            NotificationTarget::Character(char_id),
            OnGodmaFlushLocationProfile {
                location_id: ship_id,
                profile_id: PyRep::Tuple(vec![
                    PyRep::String("effect".into()),
                    PyRep::Integer(module_id),
                ]),
            },
        );
    }
    flush_module_ewar_end(ctx, char_id, solar_system_id, ship_id, module_id)
}

/// Flushes all active EWAR projections and remote assistance originating from OR targeting `ship_id`,
/// emitting `OnEwarEnd` to both attacker and victim.
pub fn flush_ship_all_ewar_sync(
    ctx: &ServerContext,
    char_id: i64,
    solar_system_id: Option<u32>,
    ship_id: i64,
) -> Vec<(i64, i64)> {
    let mut pairs: Vec<(i64, i64)> = Vec::new();
    {
        let guard = ctx
            .active_ewar_starts
            .read()
            .unwrap_or_else(|e| e.into_inner());
        for (&(src_sid, mod_id), targets) in guard.iter() {
            if src_sid == ship_id || targets.iter().any(|&(tid, _)| tid == ship_id) {
                if !pairs.contains(&(src_sid, mod_id)) {
                    pairs.push((src_sid, mod_id));
                }
            }
        }
    }
    {
        let mut target_mod_ids = Vec::new();
        {
            let src_guard = ctx
                .projected_ewar_sources
                .read()
                .unwrap_or_else(|e| e.into_inner());
            for (&mod_id, &src_sid) in src_guard.iter() {
                if src_sid == ship_id && !pairs.contains(&(src_sid, mod_id)) {
                    pairs.push((src_sid, mod_id));
                }
            }
        }
        {
            let pe_guard = ctx
                .projected_ewar
                .read()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(list) = pe_guard.get(&ship_id) {
                for eff in list {
                    target_mod_ids.push(eff.source_module_id);
                }
            }
        }
        let mut unresolved = Vec::new();
        if !target_mod_ids.is_empty() {
            let src_guard = ctx
                .projected_ewar_sources
                .read()
                .unwrap_or_else(|e| e.into_inner());
            for mod_id in target_mod_ids {
                if let Some(&src_sid) = src_guard.get(&mod_id) {
                    if src_sid > 0 && !pairs.contains(&(src_sid, mod_id)) {
                        pairs.push((src_sid, mod_id));
                    }
                } else {
                    unresolved.push(mod_id);
                }
            }
        }
        for mod_id in unresolved {
            let src_sid = ctx
                .db
                .with_snapshot(|s| s.items.get(&mod_id).map(|i| i.location_id))
                .unwrap_or(0);
            if src_sid > 0 && !pairs.contains(&(src_sid, mod_id)) {
                pairs.push((src_sid, mod_id));
            }
        }
    }
    for &(src_sid, mod_id) in &pairs {
        let src_char = if src_sid == ship_id && char_id > 0 {
            char_id
        } else {
            ctx.db
                .with_snapshot(|s| s.items.get(&src_sid).map(|i| i.owner_id))
                .unwrap_or(0)
        };
        let _ = flush_module_ewar_end(ctx, src_char, solar_system_id, src_sid, mod_id);
        crate::services::tactical_modules::remove_projected_remote_assistance(ctx, src_sid, mod_id);
    }
    pairs
}

/// Emits `OnJamStart(sourceBallID, moduleID, targetBallID, "electronic", startTime, durationMs)`
/// when an ECM jam cycle lands on `target_id`.
pub fn emit_ecm_jam_landed(
    ctx: &ServerContext,
    sys_id: u32,
    _attacker_char_id: i64,
    attacker_ship_id: i64,
    module_id: i64,
    target_id: i64,
    duration_s: f64,
) {
    let now_ft = ctx.now_client_sim_filetime_ticks();
    let duration_ms = (duration_s.max(1.0)) * 1_000.0;
    ctx.emit_typed(
        NotificationTarget::SolarSystem(sys_id),
        OnJamStart {
            source_ball_id: attacker_ship_id,
            module_id,
            target_ball_id: target_id,
            jamming_type: "electronic".into(),
            start_time: now_ft,
            duration: duration_ms,
        },
    );
}

/// Emits `OnFleetBoosted(moduleTypeID, numBoosted)` to the command-burst activator.
pub fn emit_fleet_boosted(
    ctx: &ServerContext,
    char_id: i64,
    module_type_id: u32,
    num_boosted: usize,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnFleetBoosted {
            module_type_id: i64::from(module_type_id),
            num_boosted: num_boosted as i64,
        },
    );
}

/// Emits `OnFleetJumpBeaconDeployableChange(deployableID, solarsystemID, beaconID, ownerID, active)`
/// to `fleet_id` when a Mobile Cynosural Beacon (`group_id == 4093`) is anchored or scooped.
pub fn emit_fleet_jump_beacon_deployable_change(
    ctx: &ServerContext,
    fleet_id_opt: Option<i64>,
    item_id: i64,
    sys_id: u32,
    owner_char_id: i64,
    active: bool,
) {
    let Some(fleet_id) = fleet_id_opt.filter(|&f| f > 0) else {
        return;
    };
    ctx.emit_typed(
        NotificationTarget::Fleet(fleet_id),
        OnFleetJumpBeaconDeployableChange {
            deployable_id: item_id,
            solar_system_id: i64::from(sys_id),
            beacon_id: item_id,
            owner_id: owner_char_id,
            active,
        },
    );
}

/// Emits `OnInfluenceUpdate(taleID, newInfluenceData)` across the incursion's constellation systems.
pub fn emit_incursion_influence_update(
    ctx: &ServerContext,
    constellation_id: u32,
    new_influence: f64,
) {
    let now_ft = ctx.now_blueos_filetime_ticks();
    let data = PyRep::keyval([
        ("influence", PyRep::Real(new_influence.clamp(0.0, 1.0))),
        ("lastUpdated", PyRep::Integer(now_ft)),
    ]);
    let tale_id = i64::from(constellation_id);
    for (&sys_id, sys_rec) in &ctx.sde.solar_systems {
        if sys_rec.constellation_id == constellation_id {
            ctx.emit_typed(
                NotificationTarget::SolarSystem(sys_id),
                OnInfluenceUpdate {
                    tale_id,
                    new_influence_data: data.clone(),
                },
            );
        }
    }
}

/// Emits `OnJumpCloakUpdated(shipID, endTimeFt, durationFt)` (`60s` = `600_000_000` ticks)
/// when gate or wormhole jump cloak begins in the destination solar system.
pub fn emit_jump_cloak_started(ctx: &ServerContext, char_id: i64, ship_id: i64) {
    let duration_ft: i64 = 600_000_000;
    let end_ft = ctx.now_client_sim_filetime_ticks().saturating_add(duration_ft);
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnJumpCloakUpdated {
            ship_id,
            end_time: end_ft,
            duration: duration_ft,
        },
    );
}

/// Emits `OnJumpCloakCancelled(shipID)` and/or `OnInvulnCancelled(shipID)` when initiating warp
/// breaks an active gate-cloak or undock/jump invulnerability timer.
pub fn emit_warp_cloak_or_invuln_cancelled(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    had_gate_cloak: bool,
    had_invuln: bool,
) {
    if had_gate_cloak {
        ctx.emit_typed(
            NotificationTarget::Character(char_id),
            OnJumpCloakCancelled { ship_id },
        );
    }
    if had_invuln {
        ctx.emit_typed(
            NotificationTarget::Character(char_id),
            OnInvulnCancelled { ship_id },
        );
    }
}

/// Emits `OnKillNotification()` (`0` args, `id_type = "charid"`) to the victim and final-blow pilot.
pub fn emit_kill_notifications(ctx: &ServerContext, victim_char_id: i64, attacker_char_id: i64) {
    if victim_char_id > 0 {
        ctx.emit_typed(
            NotificationTarget::Character(victim_char_id),
            OnKillNotification,
        );
    }
    if attacker_char_id > 0 && attacker_char_id != victim_char_id {
        ctx.emit_typed(
            NotificationTarget::Character(attacker_char_id),
            OnKillNotification,
        );
    }
}

/// Emits `OnLocalHackProgressUpdated(hackedObjectID, hackProgress)` (`id_type = "solarsystemid"`).
pub fn emit_local_hack_progress_updated(
    ctx: &ServerContext,
    sys_id: u32,
    container_id: i64,
    progress: f64,
) {
    ctx.emit_typed(
        NotificationTarget::SolarSystem(sys_id),
        OnLocalHackProgressUpdated {
            hacked_object_id: container_id,
            hack_progress: progress.clamp(0.0, 1.0),
        },
    );
}

/// Emits `OnMatchFound()`, `OnProvingGroundsMatchFound()`, and
/// `OnAddExpiryDurationForAbyssalPvpContent(endTimeFt)` to matched pilots.
pub fn emit_pvp_match_found_and_expiry(ctx: &ServerContext, char_id: i64, expires_at_unix: u64) {
    let end_ft = (expires_at_unix as i64)
        .saturating_mul(10_000_000)
        .saturating_add(116_444_736_000_000_000);
    ctx.emit_typed(NotificationTarget::Character(char_id), OnMatchFound);
    crate::services::notify_emitters_tournaments_anomalies::emit_proving_grounds_match_found(ctx, char_id);
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnAddExpiryDurationForAbyssalPvpContent { end_time: end_ft },
    );
}

/// Emits `OnMatchMakingFailure(errors)` when `pvpFilamentMgr.JoinPVPQueue` validation fails.
pub fn emit_pvp_matchmaking_failure(ctx: &ServerContext, char_id: i64, error_code: i64) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnMatchMakingFailureCodes {
            errors: vec![error_code],
        },
    );
}

/// Emits `DoDestinyUpdates(updates)` batch destiny update notification.
pub fn emit_destiny_updates(ctx: &ServerContext, char_id: i64, updates: Vec<PyRep>) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        DoDestinyUpdates { updates },
    );
}

/// Emits `OnCloakModuleForceDeactivated(notifyMsg, notifyArgs)` upon forced decloak.
pub fn emit_cloak_module_force_deactivated(
    ctx: &ServerContext,
    char_id: i64,
    reason: &str,
    dist_m: f64,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnCloakModuleForceDeactivated {
            notify_msg: reason.to_string(),
            notify_args: PyRep::Dict(vec![
                (PyRep::String("distance".into()), PyRep::Real(dist_m)),
            ]),
        },
    );
}

/// Emits `OnComponentRemovedSystemEnvironment(solarSystemID)` when a solar system environment is cleared.
pub fn emit_component_removed_system_environment(ctx: &ServerContext, sys_id: u32) {
    ctx.emit_typed(
        NotificationTarget::SolarSystem(sys_id),
        OnComponentRemovedSystemEnvironment {
            solar_system_id: i64::from(sys_id),
        },
    );
}

/// Emits `OnDeathzoneStateUpdate(solarSystemID, shipID, trackingData)` when entering/exiting an Abyssal deathzone.
pub fn emit_deathzone_state_update(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    ship_id: i64,
    in_danger: bool,
    time_remaining_ms: f64,
) {
    let tracking_data = if in_danger {
        PyRep::Dict(vec![
            (
                PyRep::String("timeRemainingMs".into()),
                PyRep::Real(time_remaining_ms),
            ),
            (PyRep::String("dangerLevel".into()), PyRep::Real(1.0)),
        ])
    } else {
        PyRep::None
    };
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnDeathzoneStateUpdate {
            solar_system_id: i64::from(sys_id),
            ship_id,
            tracking_data,
        },
    );
}

/// Emits `OnDungeonCameraInterest(itemID, speed, duration)` to focus the camera on a dungeon object.
pub fn emit_dungeon_camera_interest(
    ctx: &ServerContext,
    char_id: i64,
    item_id: i64,
    speed: f64,
    duration: f64,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnDungeonCameraInterest {
            item_id,
            speed,
            duration,
        },
    );
}

/// Emits `OnDungeonTriggerAmbientSound(dungeonID, soundID, entityItemID, emitterID)`.
pub fn emit_dungeon_trigger_ambient_sound(
    ctx: &ServerContext,
    char_id: i64,
    dungeon_id: i64,
    sound_id: &str,
    entity_item_id: i64,
    emitter_id: i64,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnDungeonTriggerAmbientSoundStr {
            dungeon_id,
            sound_id: sound_id.to_string(),
            entity_item_id,
            emitter_id,
        },
    );
}

/// Emits `OnDungeonTriggerAudio(dungeonID, audioUrl)`.
pub fn emit_dungeon_trigger_audio(
    ctx: &ServerContext,
    char_id: i64,
    dungeon_id: i64,
    audio_url: &str,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnDungeonTriggerAudio {
            dungeon_id,
            audio_url: audio_url.to_string(),
        },
    );
}

/// Emits `OnDungeonTriggerMessage(messageType, messageID)`.
pub fn emit_dungeon_trigger_message(
    ctx: &ServerContext,
    char_id: i64,
    message_type: &str,
    message_id: i64,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnDungeonTriggerMessageStr {
            message_type: message_type.to_string(),
            message_id,
        },
    );
}

/// Emits `OnDungeonTriggerMusic(dungeonID, musicTriggerID)`.
pub fn emit_dungeon_trigger_music(
    ctx: &ServerContext,
    char_id: i64,
    dungeon_id: i64,
    music_trigger_id: i64,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnDungeonTriggerMusic {
            dungeon_id,
            music_trigger_id,
        },
    );
}

/// Emits `OnEnterTournamentSystem()` to open the tournament match status window.
pub fn emit_enter_tournament_system(ctx: &ServerContext, char_id: i64) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnEnterTournamentSystem,
    );
}

/// Emits `OnLeaveTournamentSystem()` to close the tournament match status window.
pub fn emit_leave_tournament_system(ctx: &ServerContext, char_id: i64) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnLeaveTournamentSystem,
    );
}

/// Emits `OnEssReserveBankInventoryFull()` to the solar system when reserve bank capacity is reached.
pub fn emit_ess_reserve_bank_inventory_full(ctx: &ServerContext, sys_id: u32) {
    ctx.emit_typed(
        NotificationTarget::SolarSystem(sys_id),
        OnEssReserveBankInventoryFull,
    );
}

/// Emits `OnJumpQueueUpdate(solarSystemID, space, queue, jumpKeys, keyLife)`.
pub fn emit_jump_queue_update(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    space: i64,
    queue: i64,
    jump_keys: Vec<i64>,
    key_life: i64,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnJumpQueueUpdate {
            solar_system_id: i64::from(sys_id),
            space,
            queue,
            jump_keys,
            key_life,
        },
    );
}

#[cfg(test)]
#[path = "notify_emitters_space_dungeons_tests.rs"]
mod notify_emitters_space_dungeons_tests;

