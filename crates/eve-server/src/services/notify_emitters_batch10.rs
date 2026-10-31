//! `notify_emitters_batch10` — Server-side V24 notification emitters for Batch 10
//! (Proving Grounds, Tournaments, Scanning/SignalTracker, Dungeon Instances & Room Triggers,
//! Progression & Conversations, Tales, System Effects, ESS, Mining, Industry, SimClock,
//! Abyssal/Zarzakh Deathzone & WhiteOut, and Map Heatmaps).

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use crate::router::cache_notifications::PendingMachoNotification;
use crate::router::{NotificationTarget, ServerContext};
use eve_proto::PyRep;
use glam::DVec3;

// ---------------------------------------------------------------------------
// 1. Proving Grounds & Tournaments
// ---------------------------------------------------------------------------

pub fn emit_proving_grounds_match_found(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnProvingGroundsMatchFound",
        "charid",
        vec![],
    );
}

pub fn emit_pvp_matchmaking_failure(ctx: &ServerContext, char_id: i64, errors: Vec<String>) {
    let err_list = errors.into_iter().map(PyRep::String).collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnMatchMakingFailure",
        "charid",
        vec![PyRep::List(err_list)],
    );
}

pub fn emit_enter_tournament_system(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnEnterTournamentSystem",
        "charid",
        vec![],
    );
}

pub fn emit_leave_tournament_system(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnLeaveTournamentSystem",
        "charid",
        vec![],
    );
}

pub fn emit_tournament_match_state_change(
    ctx: &ServerContext,
    red_points: i64,
    blue_points: i64,
    match_status: i64,
    red_pilots: &[i64],
    blue_pilots: &[i64],
    referees: &[i64],
) {
    let mut notified = HashSet::new();
    for &cid in red_pilots {
        if cid > 0 && notified.insert(cid) {
            ctx.notify(
                NotificationTarget::Character(cid),
                "OnTournamentMatchStateChange",
                "charid",
                vec![
                    PyRep::Integer(red_points),
                    PyRep::Integer(blue_points),
                    PyRep::Integer(0),
                    PyRep::Integer(match_status),
                ],
            );
        }
    }
    for &cid in blue_pilots {
        if cid > 0 && notified.insert(cid) {
            ctx.notify(
                NotificationTarget::Character(cid),
                "OnTournamentMatchStateChange",
                "charid",
                vec![
                    PyRep::Integer(red_points),
                    PyRep::Integer(blue_points),
                    PyRep::Integer(1),
                    PyRep::Integer(match_status),
                ],
            );
        }
    }
    for &cid in referees {
        if cid > 0 && notified.insert(cid) {
            ctx.notify(
                NotificationTarget::Character(cid),
                "OnTournamentMatchStateChange",
                "charid",
                vec![
                    PyRep::Integer(red_points),
                    PyRep::Integer(blue_points),
                    PyRep::Integer(-1),
                    PyRep::Integer(match_status),
                ],
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Scanning, SignalTracker, Dungeon Instances, & Probes
// ---------------------------------------------------------------------------

pub fn emit_cosmic_anomaly_added_for_system(
    ctx: &ServerContext,
    solar_system_id: u32,
    anomaly_id: &str,
    instance_id: i64,
    dungeon_id: i64,
    dungeon_name: &str,
) {
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "OnCosmicAnomalyAdded",
        "solarsystemid",
        vec![PyRep::keyval([
            ("targetID", PyRep::String(anomaly_id.into())),
            ("instanceID", PyRep::Integer(instance_id)),
            ("dungeonID", PyRep::Integer(dungeon_id)),
            ("dungeonName", PyRep::String(dungeon_name.into())),
            ("solarSystemID", PyRep::Integer(i64::from(solar_system_id))),
        ])],
    );
}

pub fn emit_dungeon_instance_added(
    ctx: &ServerContext,
    solar_system_id: u32,
    instance_id: i64,
    dungeon_id: i64,
    sig_id: &str,
    scan_strength: f64,
) {
    let instance = PyRep::keyval([
        ("instanceID", PyRep::Integer(instance_id)),
        ("dungeonID", PyRep::Integer(dungeon_id)),
        ("signatureID", PyRep::String(sig_id.into())),
        ("scanStrengthValue", PyRep::Real(scan_strength.max(0.01))),
    ]);
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "OnDungeonInstanceAdded",
        "solarsystemid",
        vec![PyRep::Integer(i64::from(solar_system_id)), instance],
    );
}

pub fn emit_dungeon_instance_removed(
    ctx: &ServerContext,
    solar_system_id: u32,
    instance_id: i64,
    sig_id: &str,
    dungeon_id: i64,
) {
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "OnDungeonInstanceRemoved",
        "solarsystemid",
        vec![
            PyRep::Integer(i64::from(solar_system_id)),
            PyRep::Integer(instance_id),
            PyRep::String(sig_id.into()),
            PyRep::Integer(dungeon_id),
        ],
    );
}

pub fn emit_probe_warp_end(ctx: &ServerContext, char_id: i64, probe_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnProbeWarpEnd",
        "charid",
        vec![PyRep::Integer(probe_id)],
    );
}

pub fn emit_signal_tracker_full_state_for_system(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    sigs: &[eve_db::CosmicSignatureRecord],
) {
    let now_unix = ctx.now_unix();
    let (anom_dict, struct_dict) = ctx.db.with_snapshot(|snap| {
        let anoms: Vec<(PyRep, PyRep)> = snap
            .cosmic_anomalies
            .get(&sys_id)
            .map(|list| {
                list.iter()
                    .filter(|a| {
                        !a.cleared
                            && (a.lifetime_seconds == 0
                                || now_unix < a.created_unix.saturating_add(a.lifetime_seconds))
                    })
                    .map(|a| {
                        (
                            PyRep::String(a.anomaly_id.clone()),
                            PyRep::keyval([
                                ("targetID", PyRep::String(a.anomaly_id.clone())),
                                ("dungeonName", PyRep::String(a.name.clone())),
                                ("anomalyType", PyRep::String(a.anomaly_type.clone())),
                                ("scanStrength", PyRep::Real(a.signal_strength)),
                                (
                                    "position",
                                    PyRep::Tuple(vec![
                                        PyRep::Real(a.position[0]),
                                        PyRep::Real(a.position[1]),
                                        PyRep::Real(a.position[2]),
                                    ]),
                                ),
                            ]),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        let structs: Vec<(PyRep, PyRep)> = snap
            .structures
            .values()
            .filter(|st| st.solar_system_id == sys_id)
            .map(|st| {
                (
                    PyRep::Integer(st.structure_id),
                    PyRep::keyval([
                        ("itemID", PyRep::Integer(st.structure_id)),
                        ("typeID", PyRep::Integer(i64::from(st.type_id))),
                        ("name", PyRep::String(st.name.clone())),
                        (
                            "position",
                            PyRep::Tuple(vec![
                                PyRep::Real(st.position[0]),
                                PyRep::Real(st.position[1]),
                                PyRep::Real(st.position[2]),
                            ]),
                        ),
                    ]),
                )
            })
            .collect();
        (PyRep::Dict(anoms), PyRep::Dict(structs))
    });

    let sig_dict = PyRep::Dict(
        sigs.iter()
            .filter(|s| !s.collapsed)
            .map(|s| {
                (
                    PyRep::String(s.signature_id.clone()),
                    PyRep::keyval([
                        ("targetID", PyRep::String(s.signature_id.clone())),
                        ("sigGroup", PyRep::String(s.sig_group.clone())),
                        ("typeName", PyRep::String(s.type_name.clone())),
                        ("scanStrength", PyRep::Real(s.signal_strength)),
                        (
                            "position",
                            PyRep::Tuple(vec![
                                PyRep::Real(s.position[0]),
                                PyRep::Real(s.position[1]),
                                PyRep::Real(s.position[2]),
                            ]),
                        ),
                    ]),
                )
            })
            .collect(),
    );

    let static_sites: Vec<(PyRep, PyRep)> = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|sys| {
            sys.stations
                .iter()
                .map(|st| {
                    (
                        PyRep::Integer(st.station_id),
                        PyRep::keyval([
                            ("siteID", PyRep::Integer(st.station_id)),
                            ("typeID", PyRep::Integer(i64::from(st.station_type_id))),
                            (
                                "position",
                                PyRep::Tuple(vec![
                                    PyRep::Real(st.position.x),
                                    PyRep::Real(st.position.y),
                                    PyRep::Real(st.position.z),
                                ]),
                            ),
                        ]),
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    let full_state = PyRep::Tuple(vec![
        anom_dict,
        sig_dict,
        PyRep::Dict(static_sites),
        struct_dict,
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSignalTrackerFullState",
        "charid",
        vec![
            PyRep::Integer(i64::from(sys_id)),
            full_state,
            PyRep::Bool(true),
        ],
    );
}

// ---------------------------------------------------------------------------
// 3. Dungeon Room Trigger Notifications
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn push_dungeon_room_trigger_notify(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<
        &std::sync::RwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    >,
    notify_tx: Option<&tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>>,
    recipient_char_id: i64,
    event: &str,
    args: Vec<PyRep>,
) {
    if recipient_char_id <= 0 {
        return;
    }
    let (Some(pend), Some(tx)) = (pending_notifications, notify_tx) else {
        return;
    };
    let (pkt, mpkt) =
        ServerContext::build_macho_notification_packet(recipient_char_id, None, event, "charid", args.clone());
    let notif = PendingMachoNotification {
        recipient_char_id,
        event: event.to_string(),
        id_type: "charid".to_string(),
        args,
        packet: pkt,
        marshaled_packet: mpkt,
        created_filetime: db.clock().now_blueos_filetime_ticks(),
    };
    let mut queues = pend.write().unwrap_or_else(|e| e.into_inner());
    let q = queues.entry(recipient_char_id).or_default();
    if q.len() >= 256 {
        q.remove(0);
    }
    q.push(notif.clone());
    let _ = tx.send((recipient_char_id, notif));
}

#[allow(clippy::too_many_arguments)]
pub fn emit_dungeon_camera_interest_core(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<
        &std::sync::RwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    >,
    notify_tx: Option<&tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    item_id: i64,
    speed: f64,
    duration: f64,
) {
    push_dungeon_room_trigger_notify(
        db,
        pending_notifications,
        notify_tx,
        char_id,
        "OnDungeonCameraInterest",
        vec![
            PyRep::Integer(item_id),
            PyRep::Real(speed),
            PyRep::Real(duration),
        ],
    );
}

#[allow(clippy::too_many_arguments)]
pub fn emit_dungeon_trigger_ambient_sound_core(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<
        &std::sync::RwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    >,
    notify_tx: Option<&tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    dungeon_id: i64,
    sound_id: &str,
    entity_item_id: i64,
) {
    push_dungeon_room_trigger_notify(
        db,
        pending_notifications,
        notify_tx,
        char_id,
        "OnDungeonTriggerAmbientSound",
        vec![
            PyRep::Integer(dungeon_id),
            PyRep::String(sound_id.to_string()),
            PyRep::Integer(entity_item_id),
        ],
    );
}

pub fn emit_dungeon_trigger_audio_core(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<
        &std::sync::RwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    >,
    notify_tx: Option<&tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    dungeon_id: i64,
    audio_url: &str,
) {
    push_dungeon_room_trigger_notify(
        db,
        pending_notifications,
        notify_tx,
        char_id,
        "OnDungeonTriggerAudio",
        vec![
            PyRep::Integer(dungeon_id),
            PyRep::String(audio_url.to_string()),
        ],
    );
}

pub fn emit_dungeon_trigger_message_core(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<
        &std::sync::RwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    >,
    notify_tx: Option<&tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    message_type: &str,
    message_id: i64,
) {
    push_dungeon_room_trigger_notify(
        db,
        pending_notifications,
        notify_tx,
        char_id,
        "OnDungeonTriggerMessage",
        vec![
            PyRep::String(message_type.to_string()),
            PyRep::Integer(message_id),
        ],
    );
}

pub fn emit_dungeon_trigger_music_core(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<
        &std::sync::RwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    >,
    notify_tx: Option<&tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    dungeon_id: i64,
    music_trigger_id: i64,
) {
    push_dungeon_room_trigger_notify(
        db,
        pending_notifications,
        notify_tx,
        char_id,
        "OnDungeonTriggerMusic",
        vec![
            PyRep::Integer(dungeon_id),
            PyRep::Integer(music_trigger_id),
        ],
    );
}

// ---------------------------------------------------------------------------
// 4. Progression & Conversations
// ---------------------------------------------------------------------------

pub fn emit_progression_left(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnProgressionLeft",
        "charid",
        vec![],
    );
}

// ---------------------------------------------------------------------------
// 5. Tales / Incursions
// ---------------------------------------------------------------------------

pub fn emit_tale_data_for_system(
    ctx: &ServerContext,
    solar_system_id: u32,
    tale_id: i64,
    tale_payload: PyRep,
) {
    let data = PyRep::Dict(vec![(PyRep::Integer(tale_id), tale_payload)]);
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "OnTaleData",
        "solarsystemid",
        vec![PyRep::Integer(i64::from(solar_system_id)), data],
    );
}

pub fn emit_tale_influence_update(
    ctx: &ServerContext,
    constellation_id: u32,
    tale_id: i64,
    new_influence: f64,
) {
    let now_ft = ctx.now_blueos_filetime_ticks();
    let payload = PyRep::keyval([
        ("influence", PyRep::Real(new_influence)),
        ("lastUpdated", PyRep::Integer(now_ft)),
    ]);
    for (&sys_id, sys_rec) in &ctx.sde.solar_systems {
        if sys_rec.constellation_id == constellation_id {
            ctx.notify(
                NotificationTarget::SolarSystem(sys_id),
                "OnInfluenceUpdate",
                "solarsystemid",
                vec![PyRep::Integer(tale_id), payload.clone()],
            );
        }
    }
}

pub fn emit_tale_remove_for_constellation(
    ctx: &ServerContext,
    constellation_id: u32,
    tale_id: i64,
    template_class_id: i64,
    template_id: i64,
) {
    for (&sys_id, sys_rec) in &ctx.sde.solar_systems {
        if sys_rec.constellation_id == constellation_id {
            ctx.notify(
                NotificationTarget::SolarSystem(sys_id),
                "OnTaleRemove",
                "solarsystemid",
                vec![
                    PyRep::Integer(tale_id),
                    PyRep::Integer(template_class_id),
                    PyRep::Integer(template_id),
                ],
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 6. System Effects, ESS, Mining, Industry, SimClock, WhiteOut, Deathzone, Map
// ---------------------------------------------------------------------------

pub fn emit_system_effect_started(ctx: &ServerContext, solar_system_id: u32, type_ids: Vec<i64>) {
    let list = type_ids.into_iter().map(PyRep::Integer).collect();
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "OnSystemEffectStarted",
        "solarsystemid",
        vec![PyRep::List(list)],
    );
}

pub fn emit_system_wide_effect_stop(
    ctx: &ServerContext,
    solar_system_id: u32,
    effect_id: i64,
    source_item_id: i64,
    source_type_id: i64,
) {
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "OnSystemWideEffectStop",
        "solarsystemid",
        vec![
            PyRep::Integer(effect_id),
            PyRep::Integer(source_item_id),
            PyRep::Integer(source_type_id),
        ],
    );
}

pub fn emit_component_removed_system_environment(ctx: &ServerContext, solar_system_id: u32) {
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "OnComponentRemovedSystemEnvironment",
        "solarsystemid",
        vec![PyRep::Integer(i64::from(solar_system_id))],
    );
}

pub fn emit_ore_mined_by_other(
    ctx: &ServerContext,
    solar_system_id: u32,
    miner_char_id: i64,
    asteroid_id: i64,
    quantity: i64,
) {
    if quantity <= 0 {
        return;
    }
    let observer_char_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .filter(|c| {
                c.solar_system_id == solar_system_id
                    && c.character_id != miner_char_id
                    && c.station_id.is_none()
                    && c.structure_id.is_none()
                    && ctx.is_character_online(c.character_id, c.user_id)
            })
            .map(|c| c.character_id)
            .collect()
    });
    for cid in observer_char_ids {
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnOreMinedByOther",
            "charid",
            vec![PyRep::Integer(asteroid_id), PyRep::Integer(quantity)],
        );
    }
}

pub fn emit_analysis_kredits_change(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnAnalysisKreditsChange",
        "charid",
        vec![],
    );
}

pub fn emit_system_activity_updated(ctx: &ServerContext, solar_system_id: u32) {
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "OnSystemActivityUpdated",
        "solarsystemid",
        vec![],
    );
}

pub fn emit_sim_clock_rebase_if_advanced(ctx: &ServerContext, prev_unix: u64, secs: u64) {
    if secs == 0 {
        return;
    }
    let old_ft = (prev_unix as i64)
        .saturating_mul(10_000_000)
        .saturating_add(116_444_736_000_000_000);
    let new_ft = ctx.now_blueos_filetime_ticks();
    if new_ft <= old_ft {
        return;
    }
    let in_space_chars: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .filter(|c| {
                c.station_id.is_none()
                    && c.structure_id.is_none()
                    && ctx.is_character_online(c.character_id, c.user_id)
            })
            .map(|c| c.character_id)
            .collect()
    });
    let times = PyRep::Tuple(vec![PyRep::Integer(old_ft), PyRep::Integer(new_ft)]);
    for cid in in_space_chars {
        ctx.notify(
            NotificationTarget::Character(cid),
            "DoSimClockRebase",
            "charid",
            vec![times.clone()],
        );
    }
}

pub fn emit_space_white_out_start(ctx: &ServerContext, char_id: i64, solar_system_id: u32) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSpaceWhiteOutStart",
        "charid",
        vec![PyRep::Integer(i64::from(solar_system_id))],
    );
}

pub fn sync_deathzone_state_notifications(ctx: &ServerContext, sys_id: u32) {
    const BLOB_KEY: &str = "deathzone_active_ships";
    let boundary_r = crate::services::pve_abyssal_hazards::ABYSSAL_ROOM_BOUNDARY_RADIUS_M;
    if !(32_000_000..=32_999_999).contains(&sys_id) {
        return;
    }

    let handle = ctx.systems.get_or_spawn(sys_id);
    let ball_positions: HashMap<i64, DVec3> = handle
        .ballpark
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .balls
        .iter()
        .map(|(&id, b)| (id, b.position))
        .collect();

    let center_by_char: HashMap<i64, DVec3> =
        crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
            reg.abyssal_pockets
                .values()
                .filter(|p| p.abyssal_solar_system_id == sys_id && !p.completed && !p.collapsed_destroyed)
                .map(|p| {
                    let z = (p.current_room.saturating_sub(1) as f64) * 50_000.0;
                    (
                        p.character_id,
                        DVec3::from_array(p.grid_offset) + DVec3::new(0.0, 0.0, z),
                    )
                })
                .collect()
        });

    let now_ft = ctx.now_blueos_filetime_ticks();
    let transitions: Vec<(i64, i64, bool)> = ctx.db.with_mut_snapshot(|snap| {
        let mut active_set: HashSet<i64> = snap
            .service_state_blobs
            .get(BLOB_KEY)
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let mut out = Vec::new();
        for ch in snap.characters.values() {
            if ch.solar_system_id != sys_id
                || ch.station_id.is_some()
                || ch.structure_id.is_some()
                || ch.active_ship_id <= 0
            {
                continue;
            }
            let sid = ch.active_ship_id;
            let Some(&pos) = ball_positions.get(&sid) else {
                continue;
            };
            let center = center_by_char.get(&ch.character_id).copied().unwrap_or(DVec3::ZERO);
            let outside = pos.distance(center) > boundary_r;
            let was_outside = active_set.contains(&sid);
            if outside && !was_outside {
                active_set.insert(sid);
                out.push((ch.character_id, sid, true));
            } else if !outside && was_outside {
                active_set.remove(&sid);
                out.push((ch.character_id, sid, false));
            }
        }
        if let Ok(json) = serde_json::to_string(&active_set) {
            snap.service_state_blobs.insert(BLOB_KEY.to_string(), json);
        }
        out
    });

    for (cid, ship_id, entered_danger) in transitions {
        let tracking_data = if entered_danger {
            PyRep::keyval([
                ("damageStartTimestamp", PyRep::Integer(now_ft)),
                (
                    "damageMaxTimestamp",
                    PyRep::Integer(now_ft.saturating_add(150_000_000)),
                ),
                ("dangerLevel", PyRep::Real(1.0)),
            ])
        } else {
            PyRep::None
        };
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnDeathzoneStateUpdate",
            "charid",
            vec![
                PyRep::Integer(i64::from(sys_id)),
                PyRep::Integer(ship_id),
                tracking_data,
            ],
        );
    }
}
