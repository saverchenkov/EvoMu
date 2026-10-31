//! `notify_emitters_batch3_space` — Server-side V24 notification emitters for Batch 3
//! (Space, Destiny, ESS, PvE Dungeons/Escalations, Godma/EWAR, Drones, Crimewatch, Structures, PvP/Tourney).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_proto::{BridgeRpcError, DBRowDescriptor, PyRep};

/// Builds the V24 `OnDamageStateChange` 3-tuple:
/// `((shieldRatio, shieldTauMs, timestampFt), armorRatio, structureRatio)`.
pub fn build_damage_state_tuple(
    ctx: &ServerContext,
    item_id: i64,
    shield: f64,
    armor: f64,
    structure: f64,
) -> PyRep {
    let now_ft = ctx.now_blueos_filetime_ticks();
    let tau_ms = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&item_id).map(|i| i.type_id))
        .and_then(|tid| ctx.sde.types.get(&tid))
        .and_then(|t| t.default_attributes.get(&479).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or(100_000.0);
    PyRep::Tuple(vec![
        PyRep::Tuple(vec![
            PyRep::Real(shield.clamp(0.0, 1.0)),
            PyRep::Real(tau_ms),
            PyRep::Integer(now_ft),
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
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "OnCaptureChanged",
        "solarsystemid",
        vec![
            PyRep::Integer(ball_id),
            PyRep::Integer(faction_id),
            PyRep::Integer(now_ft),
            PyRep::Real(points),
            PyRep::Real(capture_time_ms),
            PyRep::Integer(faction_id),
        ],
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
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnCfgDataChanged",
        "solarsystemid",
        vec![
            PyRep::String("evelocations".into()),
            PyRep::List(vec![
                PyRep::Integer(structure_id),
                PyRep::String(name.to_string()),
                PyRep::Real(pos[0]),
                PyRep::Real(pos[1]),
                PyRep::Real(pos[2]),
                PyRep::None,
            ]),
        ],
    );
    if type_id == 37_534 {
        let sim_time = if online {
            PyRep::Integer(ctx.now_blueos_filetime_ticks())
        } else {
            PyRep::None
        };
        ctx.notify(
            NotificationTarget::SolarSystem(sys_id),
            "OnCynoJammerChanged",
            "solarsystemid",
            vec![PyRep::Integer(i64::from(sys_id)), sim_time],
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
            ctx.notify(
                NotificationTarget::SolarSystem(sys_id),
                "OnESSMainBankLinkNotification",
                "solarsystemid",
                vec![PyRep::Integer(i64::from(sys_id))],
            );
        } else if payout > 0.0 {
            ctx.notify(
                NotificationTarget::SolarSystem(sys_id),
                "OnMainBankPlayerHackSuccess",
                "solarsystemid",
                vec![PyRep::Dict(vec![
                    (
                        PyRep::String("solarSystemID".into()),
                        PyRep::Integer(i64::from(sys_id)),
                    ),
                    (PyRep::String("linkID".into()), PyRep::Integer(1)),
                    (PyRep::String("characterID".into()), PyRep::Integer(char_id)),
                    (PyRep::String("amount".into()), PyRep::Real(payout)),
                ])],
            );
        }
    } else if method == "AttemptLinkToReserveBank" && link_started_only {
        ctx.notify(
            NotificationTarget::SolarSystem(sys_id),
            "OnESSReserveBankLinkNotification",
            "solarsystemid",
            vec![PyRep::Integer(i64::from(sys_id))],
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
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnMainBankPlayerLinkDisconnected",
        "solarsystemid",
        vec![PyRep::Dict(vec![
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
        ])],
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
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnCosmicAnomalyAdded",
        "solarsystemid",
        vec![PyRep::keyval([
            ("targetID", PyRep::String(out_anom_id.clone())),
            ("instanceID", PyRep::Integer(out_inst_id)),
            ("dungeonID", PyRep::Integer(i64::from(dungeon_id))),
            ("dungeonName", PyRep::String(out_name)),
            ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
        ])],
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
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnCosmicSignatureAdded",
        "solarsystemid",
        vec![PyRep::String(sig_id.to_string())],
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
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnEscalatingPathChange",
        "charid",
        vec![],
    );
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnEscalatingPathMessage",
        "charid",
        vec![PyRep::Integer(instance_id)],
    );
    if notify_fleet {
        let fleet_id_opt = ctx
            .db
            .with_snapshot(|s| s.characters.get(&char_id).and_then(|c| c.fleet_id));
        if let Some(fid) = fleet_id_opt.filter(|&f| f > 0) {
            ctx.notify(
                NotificationTarget::Fleet(fid),
                "OnFleetEscalatingPathMessage",
                "fleetid",
                vec![
                    PyRep::Integer(char_id),
                    PyRep::Integer(i64::from(dungeon_template_id)),
                ],
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
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnEnteringDungeonRoom",
        "charid",
        vec![
            PyRep::Integer(dungeon_id),
            PyRep::Integer(room_id),
            PyRep::Tuple(vec![
                PyRep::Real(room_pos[0]),
                PyRep::Real(room_pos[1]),
                PyRep::Real(room_pos[2]),
            ]),
            PyRep::Integer(instance_id),
            values_dict.clone(),
        ],
    );
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDungeonValues",
        "charid",
        vec![PyRep::Integer(dungeon_id), values_dict],
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
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnEscalatingPathDungeonEntered",
            "charid",
            vec![row],
        );
    } else {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnDistributionDungeonEntered",
            "charid",
            vec![row],
        );
    }

    if completed_now {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnDungeonCompleted",
            "charid",
            vec![PyRep::Integer(i64::from(dungeon_id))],
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
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnExitingDungeon",
            "charid",
            vec![PyRep::Integer(i64::from(dungeon_id))],
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
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDungeonEdit",
        "charid",
        vec![
            PyRep::Integer(dungeon_id),
            PyRep::Integer(room_id),
            PyRep::Tuple(vec![
                PyRep::Real(room_pos[0]),
                PyRep::Real(room_pos[1]),
                PyRep::Real(room_pos[2]),
            ]),
        ],
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
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDroneActivityChange",
        "charid",
        vec![
            PyRep::Integer(drone_id),
            PyRep::Integer(activity_id),
            activity
                .map(|s| PyRep::String(s.to_string()))
                .unwrap_or(PyRep::None),
        ],
    );
}

/// Emits `OnGodmaEffectInfo`, `OnGodmaPrimeItem`, and `OnEffectMessage` when a module activates.
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
    let now_ft = ctx.now_blueos_filetime_ticks();
    let effect_id = crate::router::binding::effect_id_for_module_sde(
        &ctx.sde,
        module_type_id,
        Some(effect_name),
    )
    .unwrap_or(16);
    let dur_ms = (duration_s.max(1.0)) * 1_000.0;
    let active_effect_line = PyRep::List(vec![
        PyRep::Integer(module_id),
        PyRep::Integer(effect_id),
        PyRep::Integer(now_ft),
        PyRep::Real(dur_ms),
        PyRep::Integer(0),
        PyRep::Tuple(vec![
            PyRep::Integer(module_id),
            PyRep::Integer(char_id),
            PyRep::Integer(ship_id),
            target_id_opt.map(PyRep::Integer).unwrap_or(PyRep::None),
            PyRep::None,
            PyRep::List(vec![PyRep::Integer(module_id)]),
            PyRep::Integer(effect_id),
        ]),
        PyRep::Integer(now_ft),
        PyRep::Real(dur_ms),
        PyRep::Integer(0),
        PyRep::None,
    ]);
    let active_effects = PyRep::Dict(vec![(PyRep::Integer(effect_id), active_effect_line)]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnGodmaEffectInfo",
        "charid",
        vec![
            PyRep::Integer(module_id),
            PyRep::Integer(i64::from(module_type_id)),
            active_effects.clone(),
            PyRep::Integer(now_ft),
        ],
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
        merged_attrs.entry(73).or_insert(dur_ms);
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
        ]);
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnGodmaPrimeItem",
            "charid",
            vec![PyRep::Integer(ship_id), prime_row],
        );
    }

    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnEffectMessage",
        "charid",
        vec![
            PyRep::String("ModuleEffectActivated".into()),
            PyRep::Dict(vec![
                (
                    PyRep::String("module".into()),
                    PyRep::Integer(i64::from(module_type_id)),
                ),
                (PyRep::String("duration".into()), PyRep::Real(dur_ms)),
            ]),
        ],
    );
}

/// Emits `OnGodmaFlushLocationProfile(shipID, ("effect", moduleID))` and, for projected EWAR modules,
/// `OnEwarEnd(sourceBallID, moduleID, targetBallID, jammingType)` before removing projected EWAR.
pub fn flush_module_profile_and_ewar_end(
    ctx: &ServerContext,
    char_id: i64,
    solar_system_id: Option<u32>,
    ship_id: i64,
    module_id: i64,
) -> Vec<i64> {
    if ship_id > 0 && module_id > 0 && module_id != ship_id {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnGodmaFlushLocationProfile",
            "charid",
            vec![
                PyRep::Integer(ship_id),
                PyRep::Tuple(vec![
                    PyRep::String("effect".into()),
                    PyRep::Integer(module_id),
                ]),
            ],
        );
    }
    let projected_targets: Vec<(i64, u32)> = {
        let guard = ctx
            .projected_ewar
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let mut out = Vec::new();
        for (&target_sid, list) in guard.iter() {
            for eff in list {
                if eff.source_module_id == module_id {
                    out.push((target_sid, eff.source_module_type_id));
                }
            }
        }
        out
    };
    let affected = ctx.remove_projected_ewar_by_module(module_id);
    if let Some(sys_id) = solar_system_id {
        for (target_sid, mod_tid) in projected_targets {
            let grp = ctx.sde.types.get(&mod_tid).map(|t| t.group_id).unwrap_or(0);
            let jamming_type = match grp {
                52 | 899 => "warpScrambler",
                65 | 1672 => "ewar_webify",
                379 => "ewar_target_painter",
                208 => "ewar_sensor_damp",
                291 => "ewar_tracking_disrupt",
                842 | 1245 => "ewar_guidance_disrupt",
                _ => "electronic",
            };
            ctx.notify(
                NotificationTarget::SolarSystem(sys_id),
                "OnEwarEnd",
                "solarsystemid",
                vec![
                    PyRep::Integer(ship_id),
                    PyRep::Integer(module_id),
                    PyRep::Integer(target_sid),
                    PyRep::String(jamming_type.into()),
                ],
            );
        }
    }
    affected
}

/// Emits `OnJamStart(sourceBallID, moduleID, targetBallID, "electronic", startTime, durationMs)`
/// and `OnCombatMessage` when an ECM jam cycle lands on `target_id`.
pub fn emit_ecm_jam_landed(
    ctx: &ServerContext,
    sys_id: u32,
    attacker_char_id: i64,
    attacker_ship_id: i64,
    module_id: i64,
    target_id: i64,
    duration_s: f64,
) {
    let now_ft = ctx.now_blueos_filetime_ticks();
    let duration_ms = (duration_s.max(1.0)) * 1_000.0;
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnJamStart",
        "solarsystemid",
        vec![
            PyRep::Integer(attacker_ship_id),
            PyRep::Integer(module_id),
            PyRep::Integer(target_id),
            PyRep::String("electronic".into()),
            PyRep::Integer(now_ft),
            PyRep::Real(duration_ms),
        ],
    );
    let msg_args = PyRep::Dict(vec![
        (
            PyRep::String("attacker".into()),
            PyRep::Integer(attacker_ship_id),
        ),
        (PyRep::String("target".into()), PyRep::Integer(target_id)),
    ]);
    ctx.notify(
        NotificationTarget::Character(attacker_char_id),
        "OnCombatMessage",
        "charid",
        vec![PyRep::String("EcmJamSuccess".into()), msg_args],
    );
}

/// Emits `OnFleetBoosted(moduleTypeID, numBoosted)` to the command-burst activator.
pub fn emit_fleet_boosted(
    ctx: &ServerContext,
    char_id: i64,
    module_type_id: u32,
    num_boosted: usize,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnFleetBoosted",
        "charid",
        vec![
            PyRep::Integer(i64::from(module_type_id)),
            PyRep::Integer(num_boosted as i64),
        ],
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
    ctx.notify(
        NotificationTarget::Fleet(fleet_id),
        "OnFleetJumpBeaconDeployableChange",
        "fleetid",
        vec![
            PyRep::Integer(item_id),
            PyRep::Integer(i64::from(sys_id)),
            PyRep::Integer(item_id),
            PyRep::Integer(owner_char_id),
            PyRep::Bool(active),
        ],
    );
}

/// Validates and registers an Equinox Mercenary Den (`type_id == 85_230`) during `posMgr.AnchorStructure`,
/// emitting `OnMercenaryDenRegistrationInvalid` on failure or `OnMercenaryDenRegistrationSuccess` on success.
pub fn validate_and_register_mercenary_den(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    sys_sec: f64,
    item_id: i64,
) -> Result<(), BridgeRpcError> {
    let now_unix = ctx.now_unix();
    let check_res = ctx.db.try_with_mut_snapshot(|snap| {
        if sys_sec > 0.0 {
            return Err("Mercenary Dens can only be anchored in sovereign null-security space".to_string());
        }
        let Some(skyhook_id) = snap
            .structures
            .values()
            .find(|s| {
                s.solar_system_id == sys_id && s.type_id == 81_080 && s.state != "destroyed"
            })
            .map(|s| s.structure_id)
        else {
            return Err("Mercenary Den requires an active Orbital Skyhook (81080) in solar system".to_string());
        };
        let skill_lvl = snap
            .skills
            .get(&char_id)
            .and_then(|m| m.get(&85_233))
            .map(|s| u64::from(s.level))
            .unwrap_or(0);
        let owned_count = snap
            .sov_runtime
            .mercenary_dens
            .values()
            .filter(|d| d.owner_char_id == char_id)
            .count() as u64;
        if skill_lvl == 0 || owned_count >= skill_lvl.min(5) {
            return Err("Mercenary Den Management (85233) skill limit exceeded".to_string());
        }
        let planet_id = ctx
            .sde
            .celestials_in_system(sys_id)
            .into_iter()
            .find(|c| c.group_id == 7)
            .map(|c| c.celestial_id)
            .unwrap_or(40_000_001);
        snap.sov_runtime.mercenary_dens.insert(
            item_id,
            eve_db::PersistedMercenaryDen {
                den_id: item_id,
                type_id: 85_230,
                owner_char_id: char_id,
                skyhook_id,
                solar_system_id: sys_id,
                planet_id,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 0,
                anarchy_level: 0,
                infomorphs: 0,
                simulated_at_unix: now_unix,
                next_activity_generation_unix: now_unix + 86_400,
            },
        );
        Ok(planet_id)
    });
    match check_res {
        Ok(planet_id) => {
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnMercenaryDenRegistrationSuccess",
                "charid",
                vec![
                    PyRep::String("MercenaryDenRegistrationSuccess".into()),
                    PyRep::Dict(vec![
                        (PyRep::String("itemID".into()), PyRep::Integer(item_id)),
                        (
                            PyRep::String("solarSystemID".into()),
                            PyRep::Integer(i64::from(sys_id)),
                        ),
                        (PyRep::String("planetID".into()), PyRep::Integer(planet_id)),
                    ]),
                ],
            );
            Ok(())
        }
        Err(msg) => {
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnMercenaryDenRegistrationInvalid",
                "charid",
                vec![
                    PyRep::String("MercenaryDenRegistrationInvalid".into()),
                    PyRep::Dict(vec![
                        (PyRep::String("itemID".into()), PyRep::Integer(item_id)),
                        (
                            PyRep::String("solarSystemID".into()),
                            PyRep::Integer(i64::from(sys_id)),
                        ),
                    ]),
                ],
            );
            Err(BridgeRpcError {
                code: "MercenaryDenRegistrationInvalid".into(),
                message: msg.to_string(),
            })
        }
    }
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
            ctx.notify(
                NotificationTarget::SolarSystem(sys_id),
                "OnInfluenceUpdate",
                "solarsystemid",
                vec![PyRep::Integer(tale_id), data.clone()],
            );
        }
    }
}

/// Emits `OnJumpCloakUpdated(shipID, endTimeFt, durationFt)` (`60s` = `600_000_000` ticks)
/// when gate or wormhole jump cloak begins in the destination solar system.
pub fn emit_jump_cloak_started(ctx: &ServerContext, char_id: i64, ship_id: i64) {
    let duration_ft: i64 = 600_000_000;
    let end_ft = ctx.now_blueos_filetime_ticks().saturating_add(duration_ft);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnJumpCloakUpdated",
        "charid",
        vec![
            PyRep::Integer(ship_id),
            PyRep::Integer(end_ft),
            PyRep::Integer(duration_ft),
        ],
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
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnJumpCloakCancelled",
            "charid",
            vec![PyRep::Integer(ship_id)],
        );
    }
    if had_invuln {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnInvulnCancelled",
            "charid",
            vec![PyRep::Integer(ship_id)],
        );
    }
}

/// Emits `OnKillNotification()` (`0` args, `id_type = "charid"`) to the victim and final-blow pilot.
pub fn emit_kill_notifications(ctx: &ServerContext, victim_char_id: i64, attacker_char_id: i64) {
    if victim_char_id > 0 {
        ctx.notify(
            NotificationTarget::Character(victim_char_id),
            "OnKillNotification",
            "charid",
            vec![],
        );
    }
    if attacker_char_id > 0 && attacker_char_id != victim_char_id {
        ctx.notify(
            NotificationTarget::Character(attacker_char_id),
            "OnKillNotification",
            "charid",
            vec![],
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
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnLocalHackProgressUpdated",
        "solarsystemid",
        vec![
            PyRep::Integer(container_id),
            PyRep::Real(progress.clamp(0.0, 1.0)),
        ],
    );
}

/// Emits `OnMatchFound()` and `OnAddExpiryDurationForAbyssalPvpContent(endTimeFt)` to matched pilots.
pub fn emit_pvp_match_found_and_expiry(ctx: &ServerContext, char_id: i64, expires_at_unix: u64) {
    let end_ft = (expires_at_unix as i64)
        .saturating_mul(10_000_000)
        .saturating_add(116_444_736_000_000_000);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnMatchFound",
        "charid",
        vec![],
    );
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnAddExpiryDurationForAbyssalPvpContent",
        "charid",
        vec![PyRep::Integer(end_ft)],
    );
}

/// Emits `OnMatchMakingFailure(errors)` when `pvpFilamentMgr.JoinPVPQueue` validation fails.
pub fn emit_pvp_matchmaking_failure(ctx: &ServerContext, char_id: i64, error_code: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnMatchMakingFailure",
        "charid",
        vec![PyRep::List(vec![PyRep::Integer(error_code)])],
    );
}

/// Emits `DoDestinyUpdates(updates)` batch destiny update notification.
pub fn emit_destiny_updates(ctx: &ServerContext, char_id: i64, updates: Vec<PyRep>) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "DoDestinyUpdates",
        "charid",
        vec![PyRep::List(updates)],
    );
}

/// Emits `OnCloakModuleForceDeactivated(notifyMsg, notifyArgs)` upon forced decloak.
pub fn emit_cloak_module_force_deactivated(
    ctx: &ServerContext,
    char_id: i64,
    reason: &str,
    dist_m: f64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnCloakModuleForceDeactivated",
        "charid",
        vec![
            PyRep::String(reason.to_string()),
            PyRep::Dict(vec![
                (PyRep::String("distance".into()), PyRep::Real(dist_m)),
            ]),
        ],
    );
}

/// Emits `OnComponentRemovedSystemEnvironment(solarSystemID)` when a solar system environment is cleared.
pub fn emit_component_removed_system_environment(ctx: &ServerContext, sys_id: u32) {
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnComponentRemovedSystemEnvironment",
        "solarsystemid",
        vec![PyRep::Integer(i64::from(sys_id))],
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
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDeathzoneStateUpdate",
        "charid",
        vec![
            PyRep::Integer(i64::from(sys_id)),
            PyRep::Integer(ship_id),
            tracking_data,
        ],
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
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDungeonCameraInterest",
        "charid",
        vec![
            PyRep::Integer(item_id),
            PyRep::Real(speed),
            PyRep::Real(duration),
        ],
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
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDungeonTriggerAmbientSound",
        "charid",
        vec![
            PyRep::Integer(dungeon_id),
            PyRep::String(sound_id.to_string()),
            PyRep::Integer(entity_item_id),
            PyRep::Integer(emitter_id),
        ],
    );
}

/// Emits `OnDungeonTriggerAudio(dungeonID, audioUrl)`.
pub fn emit_dungeon_trigger_audio(
    ctx: &ServerContext,
    char_id: i64,
    dungeon_id: i64,
    audio_url: &str,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDungeonTriggerAudio",
        "charid",
        vec![
            PyRep::Integer(dungeon_id),
            PyRep::String(audio_url.to_string()),
        ],
    );
}

/// Emits `OnDungeonTriggerMessage(messageType, messageID)`.
pub fn emit_dungeon_trigger_message(
    ctx: &ServerContext,
    char_id: i64,
    message_type: &str,
    message_id: i64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDungeonTriggerMessage",
        "charid",
        vec![
            PyRep::String(message_type.to_string()),
            PyRep::Integer(message_id),
        ],
    );
}

/// Emits `OnDungeonTriggerMusic(dungeonID, musicTriggerID)`.
pub fn emit_dungeon_trigger_music(
    ctx: &ServerContext,
    char_id: i64,
    dungeon_id: i64,
    music_trigger_id: i64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDungeonTriggerMusic",
        "charid",
        vec![
            PyRep::Integer(dungeon_id),
            PyRep::Integer(music_trigger_id),
        ],
    );
}

/// Emits `OnEnterTournamentSystem()` to open the tournament match status window.
pub fn emit_enter_tournament_system(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnEnterTournamentSystem",
        "charid",
        vec![],
    );
}

/// Emits `OnLeaveTournamentSystem()` to close the tournament match status window.
pub fn emit_leave_tournament_system(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnLeaveTournamentSystem",
        "charid",
        vec![],
    );
}

/// Emits `OnEssReserveBankInventoryFull()` to the solar system when reserve bank capacity is reached.
pub fn emit_ess_reserve_bank_inventory_full(ctx: &ServerContext, sys_id: u32) {
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnEssReserveBankInventoryFull",
        "solarsystemid",
        vec![],
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
    let keys = jump_keys.into_iter().map(PyRep::Integer).collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnJumpQueueUpdate",
        "charid",
        vec![
            PyRep::Integer(i64::from(sys_id)),
            PyRep::Integer(space),
            PyRep::Integer(queue),
            PyRep::List(keys),
            PyRep::Integer(key_life),
        ],
    );
}
