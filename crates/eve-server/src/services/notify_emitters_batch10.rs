//! `notify_emitters_batch10` — Server-side V24 notification emitters for Batch 10
//! (Dungeons, Escalating Paths, Tales, Anomalies/Signatures, Abyssal/Void/Tournaments, ESS/FW/System Effects).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_proto::PyRep;

// ---------------------------------------------------------------------------
// 1. Proving Grounds & Tournaments
// ---------------------------------------------------------------------------

pub fn emit_pvp_match_found_bundle(ctx: &ServerContext, char_id: i64, end_ft: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnMatchFound",
        "charid",
        vec![],
    );
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnProvingGroundsMatchFound",
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
    char_id: i64,
    red_points: i64,
    blue_points: i64,
    my_team: i64,
    match_status: i64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnTournamentMatchStateChange",
        "charid",
        vec![
            PyRep::Integer(red_points),
            PyRep::Integer(blue_points),
            PyRep::Integer(my_team),
            PyRep::Integer(match_status),
        ],
    );
}

// ---------------------------------------------------------------------------
// 2. Dungeons & Escalating Paths
// ---------------------------------------------------------------------------

pub fn emit_distribution_dungeon_entered(
    ctx: &ServerContext,
    char_id: i64,
    dungeon_id: i64,
    dungeon_name: &str,
    dungeon_name_id: i64,
    solar_system_id: i64,
) {
    let row = PyRep::keyval([
        (
            "header",
            PyRep::List(vec![
                PyRep::String("dungeonID".into()),
                PyRep::String("dungeonName".into()),
                PyRep::String("dungeonNameID".into()),
                PyRep::String("solarSystemID".into()),
            ]),
        ),
        ("dungeonID", PyRep::Integer(dungeon_id)),
        ("dungeonName", PyRep::String(dungeon_name.into())),
        ("dungeonNameID", PyRep::Integer(dungeon_name_id)),
        ("solarSystemID", PyRep::Integer(solar_system_id)),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDistributionDungeonEntered",
        "charid",
        vec![row],
    );
}

pub fn emit_escalating_path_dungeon_entered(
    ctx: &ServerContext,
    char_id: i64,
    dungeon_id: i64,
    dungeon_name: &str,
    dungeon_name_id: i64,
    solar_system_id: i64,
) {
    let row = PyRep::keyval([
        (
            "header",
            PyRep::List(vec![
                PyRep::String("dungeonID".into()),
                PyRep::String("dungeonName".into()),
                PyRep::String("dungeonNameID".into()),
                PyRep::String("solarSystemID".into()),
            ]),
        ),
        ("dungeonID", PyRep::Integer(dungeon_id)),
        ("dungeonName", PyRep::String(dungeon_name.into())),
        ("dungeonNameID", PyRep::Integer(dungeon_name_id)),
        ("solarSystemID", PyRep::Integer(solar_system_id)),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnEscalatingPathDungeonEntered",
        "charid",
        vec![row],
    );
}

pub fn emit_escalating_path_change(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnEscalatingPathChange",
        "charid",
        vec![],
    );
}

#[allow(clippy::too_many_arguments)]
pub fn emit_entering_dungeon_room(
    ctx: &ServerContext,
    char_id: i64,
    dungeon_id: i64,
    room_id: i64,
    room_pos: [f64; 3],
    instance_id: i64,
    values: Vec<(String, PyRep)>,
) {
    let pos_tuple = PyRep::Tuple(vec![
        PyRep::Real(room_pos[0]),
        PyRep::Real(room_pos[1]),
        PyRep::Real(room_pos[2]),
    ]);
    let val_dict = PyRep::Dict(
        values
            .into_iter()
            .map(|(k, v)| (PyRep::String(k), v))
            .collect(),
    );
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnEnteringDungeonRoom",
        "charid",
        vec![
            PyRep::Integer(dungeon_id),
            PyRep::Integer(room_id),
            pos_tuple,
            PyRep::Integer(instance_id),
            val_dict,
        ],
    );
}

pub fn emit_exiting_dungeon(ctx: &ServerContext, char_id: i64, dungeon_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnExitingDungeon",
        "charid",
        vec![PyRep::Integer(dungeon_id)],
    );
}

pub fn emit_dungeon_completed(ctx: &ServerContext, char_id: i64, dungeon_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDungeonCompleted",
        "charid",
        vec![PyRep::Integer(dungeon_id)],
    );
}

pub fn emit_dungeon_values(
    ctx: &ServerContext,
    char_id: i64,
    dungeon_id: i64,
    values: Vec<(String, PyRep)>,
) {
    let val_dict = PyRep::Dict(
        values
            .into_iter()
            .map(|(k, v)| (PyRep::String(k), v))
            .collect(),
    );
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDungeonValues",
        "charid",
        vec![PyRep::Integer(dungeon_id), val_dict],
    );
}

pub fn emit_dungeon_camera_interest(
    ctx: &ServerContext,
    char_id: i64,
    item_id: i64,
    speed: f64,
    duration_ms: i64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDungeonCameraInterest",
        "charid",
        vec![
            PyRep::Integer(item_id),
            PyRep::Real(speed),
            PyRep::Integer(duration_ms),
        ],
    );
}

pub fn emit_dungeon_edit(
    ctx: &ServerContext,
    char_id: i64,
    dungeon_id: i64,
    room_id: i64,
    room_pos: [f64; 3],
) {
    let pos_tuple = PyRep::Tuple(vec![
        PyRep::Real(room_pos[0]),
        PyRep::Real(room_pos[1]),
        PyRep::Real(room_pos[2]),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDungeonEdit",
        "charid",
        vec![
            PyRep::Integer(dungeon_id),
            PyRep::Integer(room_id),
            pos_tuple,
        ],
    );
}

pub fn emit_dungeon_trigger_ambient_sound(
    ctx: &ServerContext,
    char_id: i64,
    dungeon_id: i64,
    sound_id: i64,
    entity_item_id: i64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDungeonTriggerAmbientSound",
        "charid",
        vec![
            PyRep::Integer(dungeon_id),
            PyRep::Integer(sound_id),
            PyRep::Integer(entity_item_id),
        ],
    );
}

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
            PyRep::String(audio_url.into()),
        ],
    );
}

pub fn emit_dungeon_trigger_message(
    ctx: &ServerContext,
    char_id: i64,
    message_type: i64,
    message_id: i64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDungeonTriggerMessage",
        "charid",
        vec![PyRep::Integer(message_type), PyRep::Integer(message_id)],
    );
}

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

// ---------------------------------------------------------------------------
// 3. Anomalies, Signatures, Signal Tracker & Probes
// ---------------------------------------------------------------------------

pub fn emit_cosmic_anomaly_added(
    ctx: &ServerContext,
    solar_system_id: u32,
    target_id: &str,
    dungeon_id: i64,
    dungeon_name: &str,
) {
    let anomaly = PyRep::keyval([
        ("targetID", PyRep::String(target_id.into())),
        ("dungeonID", PyRep::Integer(dungeon_id)),
        ("dungeonName", PyRep::String(dungeon_name.into())),
        ("scanStrengthValue", PyRep::Real(1.0)),
    ]);
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "OnCosmicAnomalyAdded",
        "solarsystemid",
        vec![anomaly],
    );
}

pub fn emit_cosmic_signature_added(ctx: &ServerContext, solar_system_id: u32, target_id: &str) {
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "OnCosmicSignatureAdded",
        "solarsystemid",
        vec![PyRep::String(target_id.into())],
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
        ("scanStrengthValue", PyRep::Real(scan_strength)),
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

#[allow(clippy::too_many_arguments)]
pub fn emit_signal_tracker_full_state(
    ctx: &ServerContext,
    char_id: i64,
    solar_system_id: u32,
    anomalies: Vec<PyRep>,
    signatures: Vec<PyRep>,
    static_sites: Vec<PyRep>,
    structures: Vec<PyRep>,
    should_remove_old: bool,
) {
    let full_state = PyRep::Tuple(vec![
        PyRep::List(anomalies),
        PyRep::List(signatures),
        PyRep::List(static_sites),
        PyRep::List(structures),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSignalTrackerFullState",
        "charid",
        vec![
            PyRep::Integer(i64::from(solar_system_id)),
            full_state,
            PyRep::Bool(should_remove_old),
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

// ---------------------------------------------------------------------------
// 4. Progression & Conversations
// ---------------------------------------------------------------------------

pub fn emit_progression_room_joined(
    ctx: &ServerContext,
    char_id: i64,
    prog_name_id: i64,
    objective_id: i64,
    task_widgets: Vec<PyRep>,
    obj_history: Vec<PyRep>,
    room_id: i64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnProgressionRoomJoined",
        "charid",
        vec![
            PyRep::Integer(prog_name_id),
            PyRep::Integer(objective_id),
            PyRep::List(task_widgets),
            PyRep::List(obj_history),
            PyRep::Integer(room_id),
        ],
    );
}

pub fn emit_progression_advanced(
    ctx: &ServerContext,
    char_id: i64,
    objective_id: i64,
    last_completed_task_id: i64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnProgressionAdvanced",
        "charid",
        vec![
            PyRep::Integer(objective_id),
            PyRep::Integer(last_completed_task_id),
        ],
    );
}

pub fn emit_progression_task_completed(ctx: &ServerContext, char_id: i64, task_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnProgressionTaskCompleted",
        "charid",
        vec![PyRep::Integer(task_id)],
    );
}

pub fn emit_progression_task_widget_update(
    ctx: &ServerContext,
    char_id: i64,
    task_id: i64,
    task_widget_id: i64,
    widget_state: PyRep,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnProgressionTaskWidgetUpdate",
        "charid",
        vec![
            PyRep::Integer(task_id),
            PyRep::Integer(task_widget_id),
            widget_state,
        ],
    );
}

pub fn emit_progression_left(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnProgressionLeft",
        "charid",
        vec![],
    );
}

pub fn emit_start_conversation(
    ctx: &ServerContext,
    char_id: i64,
    conversation_id: i64,
    show_continue_button: bool,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnStartConversation",
        "charid",
        vec![
            PyRep::Integer(conversation_id),
            PyRep::Bool(show_continue_button),
        ],
    );
}

// ---------------------------------------------------------------------------
// 5. Tales, Incursions, System Effects & Dynamic Bounty / ESS / FW / Hacking
// ---------------------------------------------------------------------------

pub fn emit_tale_data(
    ctx: &ServerContext,
    char_id: i64,
    solar_system_id: u32,
    tales: Vec<(i64, PyRep)>,
) {
    let data = PyRep::Dict(
        tales
            .into_iter()
            .map(|(tid, val)| (PyRep::Integer(tid), val))
            .collect(),
    );
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnTaleData",
        "charid",
        vec![PyRep::Integer(i64::from(solar_system_id)), data],
    );
}

pub fn emit_tale_remove(
    ctx: &ServerContext,
    solar_system_id: u32,
    tale_id: i64,
    template_class_id: i64,
    template_id: i64,
) {
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "OnTaleRemove",
        "solarsystemid",
        vec![
            PyRep::Integer(tale_id),
            PyRep::Integer(template_class_id),
            PyRep::Integer(template_id),
        ],
    );
}

pub fn emit_influence_update(
    ctx: &ServerContext,
    solar_system_id: u32,
    tale_id: i64,
    influence: f64,
    last_updated_ft: i64,
) {
    let new_influence_data = PyRep::keyval([
        ("influence", PyRep::Real(influence)),
        ("lastUpdated", PyRep::Integer(last_updated_ft)),
    ]);
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "OnInfluenceUpdate",
        "solarsystemid",
        vec![PyRep::Integer(tale_id), new_influence_data],
    );
}

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

pub fn emit_system_dynamic_bounty_state_changed(
    ctx: &ServerContext,
    solar_system_id: u32,
    rate: f64,
) {
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "OnSystemDynamicBountyStateChanged",
        "solarsystemid",
        vec![PyRep::Integer(i64::from(solar_system_id)), PyRep::Real(rate)],
    );
}

pub fn emit_ess_reserve_bank_inventory_full(ctx: &ServerContext, solar_system_id: u32) {
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "OnEssReserveBankInventoryFull",
        "solarsystemid",
        vec![],
    );
}

#[allow(clippy::too_many_arguments)]
pub fn emit_capture_changed(
    ctx: &ServerContext,
    solar_system_id: u32,
    ball_id: i64,
    capture_id: i64,
    last_incident: i64,
    points: f64,
    capture_time: i64,
    last_capturing: i64,
) {
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "OnCaptureChanged",
        "solarsystemid",
        vec![
            PyRep::Integer(ball_id),
            PyRep::Integer(capture_id),
            PyRep::Integer(last_incident),
            PyRep::Real(points),
            PyRep::Integer(capture_time),
            PyRep::Integer(last_capturing),
        ],
    );
}

pub fn emit_local_hack_progress_updated(
    ctx: &ServerContext,
    char_id: i64,
    hacked_object_id: i64,
    hack_progress: f64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnLocalHackProgressUpdated",
        "charid",
        vec![
            PyRep::Integer(hacked_object_id),
            PyRep::Real(hack_progress),
        ],
    );
}

// ---------------------------------------------------------------------------
// 6. Space, Destiny, Map, Mining, Kredits, Deathzone
// ---------------------------------------------------------------------------

pub fn emit_destiny_updates(ctx: &ServerContext, solar_system_id: u32, updates: Vec<PyRep>) {
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "DoDestinyUpdates",
        "solarsystemid",
        vec![PyRep::List(updates)],
    );
}

pub fn emit_sim_clock_rebase(ctx: &ServerContext, char_id: i64, old_ft: i64, new_ft: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "DoSimClockRebase",
        "charid",
        vec![PyRep::Tuple(vec![
            PyRep::Integer(old_ft),
            PyRep::Integer(new_ft),
        ])],
    );
}

pub fn emit_server_map_data_push(
    ctx: &ServerContext,
    char_id: i64,
    tricolors: Vec<(f64, f64, f64)>,
    data: Vec<(i64, f64, f64, f64, (f64, f64, f64))>,
) {
    let tri_list = tricolors
        .into_iter()
        .map(|(r, g, b)| PyRep::Tuple(vec![PyRep::Real(r), PyRep::Real(g), PyRep::Real(b)]))
        .collect();
    let data_list = data
        .into_iter()
        .map(|(sys_id, x, y, z, (r, g, b))| {
            PyRep::Tuple(vec![
                PyRep::Integer(sys_id),
                PyRep::Real(x),
                PyRep::Real(y),
                PyRep::Real(z),
                PyRep::Tuple(vec![PyRep::Real(r), PyRep::Real(g), PyRep::Real(b)]),
            ])
        })
        .collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnServerMapDataPush",
        "charid",
        vec![PyRep::List(tri_list), PyRep::List(data_list)],
    );
}

pub fn emit_system_activity_updated(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSystemActivityUpdated",
        "charid",
        vec![],
    );
}

pub fn emit_space_white_out_start(ctx: &ServerContext, char_id: i64, solar_system_id: u32) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSpaceWhiteOutStart",
        "charid",
        vec![PyRep::Integer(i64::from(solar_system_id))],
    );
}

pub fn emit_deathzone_state_update(
    ctx: &ServerContext,
    char_id: i64,
    solar_system_id: u32,
    ship_id: i64,
    tracking_data: PyRep,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDeathzoneStateUpdate",
        "charid",
        vec![
            PyRep::Integer(i64::from(solar_system_id)),
            PyRep::Integer(ship_id),
            tracking_data,
        ],
    );
}

pub fn emit_ore_mined_by_other(
    ctx: &ServerContext,
    solar_system_id: u32,
    asteroid_id: i64,
    quantity: i64,
) {
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "OnOreMinedByOther",
        "solarsystemid",
        vec![PyRep::Integer(asteroid_id), PyRep::Integer(quantity)],
    );
}

pub fn emit_analysis_kredits_change(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnAnalysisKreditsChange",
        "charid",
        vec![],
    );
}

pub fn handle_operation_progression_event(
    ctx: &ServerContext,
    char_id: i64,
    event_name: &str,
    amount: Option<i64>,
) {
    match event_name {
        "progression_room_joined" | "start_operation" => {
            emit_progression_room_joined(
                ctx,
                char_id,
                1001,
                2001,
                vec![],
                vec![],
                amount.unwrap_or(3001),
            );
            emit_start_conversation(ctx, char_id, amount.unwrap_or(5001), true);
        }
        "progression_task_widget_update" => {
            emit_progression_task_widget_update(
                ctx,
                char_id,
                101,
                201,
                PyRep::Integer(amount.unwrap_or(1)),
            );
        }
        "progression_task_completed" => {
            emit_progression_task_completed(ctx, char_id, amount.unwrap_or(101));
        }
        "progression_advanced" => {
            emit_progression_advanced(ctx, char_id, 2002, amount.unwrap_or(101));
        }
        "progression_left" => {
            emit_progression_left(ctx, char_id);
        }
        _ => {}
    }
}
