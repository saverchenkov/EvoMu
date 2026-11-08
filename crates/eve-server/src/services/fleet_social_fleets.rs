//! Fleet Services (`SO-2`): `fleetMgr`, `fleetObjectHandler`, `fleetProxy`, and `beyonce.GetFormations`.

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{
    ChatChannelRecord, FleetMemberRecord, FleetRecord, FleetRole, FleetSquadRecord, FleetWingRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

use super::fleet_social_notify as notify;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct FleetAdvertState {
    pub(crate) fleet_id: i64,
    pub(crate) leader_id: i64,
    pub(crate) fleet_name: String,
    pub(crate) description: String,
    pub(crate) solar_system_id: u32,
    pub(crate) invite_scope: u8,
    pub(crate) auto_join: bool,
    pub(crate) min_standing: f64,
    pub(crate) min_security: f64,
    pub(crate) allowed_entities: HashSet<i64>,
    pub(crate) _created_unix: u64,
}

pub(crate) use eve_db::FleetSetupRecord;

#[derive(Clone, Debug, Default)]
pub(crate) struct FleetExtrasState {
    pub(crate) fleet_names: HashMap<i64, String>,
    pub(crate) max_sizes: HashMap<i64, u16>,
    pub(crate) auto_join_squads: HashMap<i64, Option<i64>>,
    /// `(fleetID, inviteeID)` -> `(wingID, squadID, role, inviterID)`.
    pub(crate) invite_details: HashMap<(i64, i64), (i64, i64, FleetRole, i64)>,
    /// `(fleetID, inviteeID)` -> `invited_at_unix`.
    pub(crate) invite_timestamps: HashMap<(i64, i64), u64>,
    /// `character_id` -> `disconnected_at_unix`.
    pub(crate) disconnected_members: HashMap<i64, u64>,
    pub(crate) adverts: HashMap<i64, FleetAdvertState>,
    pub(crate) watchlist: HashMap<i64, HashSet<i64>>,
    pub(crate) target_tags: HashMap<i64, HashMap<i64, String>>,
    pub(crate) options: HashMap<i64, PyRep>,
    pub(crate) wing_squad_hierarchy: HashMap<i64, (Vec<i64>, Vec<i64>)>,
    pub(crate) join_requests: HashMap<i64, HashSet<i64>>,
    pub(crate) voice_enabled: HashMap<i64, bool>,
    pub(crate) loot_logging: HashMap<i64, bool>,
    pub(crate) watchlist_loaded: bool,
    pub(crate) saved_setups: HashMap<i64, HashMap<String, FleetSetupRecord>>,
    pub(crate) stored_formations: HashMap<i64, (i64, f64, f64)>,
    pub(crate) broadcast_filters: HashMap<i64, HashSet<String>>,
    pub(crate) last_broadcast: HashMap<i64, (u64, String, i64, i64, Option<i64>)>,
    pub(crate) broadcast_history: HashMap<i64, Vec<(u64, i64, String, i64, Option<i64>, Option<i64>)>>,
}

/// The fleet holding a pending invite for `char_id`; `hint` (a fleetID argument) wins when it matches.
fn invited_fleet(ctx: &ServerContext, char_id: i64, hint: Option<i64>) -> i64 {
    ctx.db.with_snapshot(|snap| {
        if let Some(f) = hint.and_then(|h| snap.fleets.get(&h)).filter(|f| f.invites.contains(&char_id)) {
            return f.fleet_id;
        }
        snap.fleets.values().find(|f| f.invites.contains(&char_id)).map(|f| f.fleet_id).unwrap_or(0)
    })
}

pub(super) fn role_to_i64(role: FleetRole) -> i64 {
    match role {
        FleetRole::FleetCommander => 1,
        FleetRole::WingCommander => 2,
        FleetRole::SquadCommander => 3,
        FleetRole::SquadMember => 4,
    }
}

pub(super) fn i64_to_role(val: i64) -> FleetRole {
    match val {
        1 => FleetRole::FleetCommander,
        2 => FleetRole::WingCommander,
        3 => FleetRole::SquadCommander,
        _ => FleetRole::SquadMember,
    }
}

pub(super) fn role_name_str(role: FleetRole) -> &'static str {
    match role {
        FleetRole::FleetCommander => "Fleet Commander",
        FleetRole::WingCommander => "Wing Commander",
        FleetRole::SquadCommander => "Squad Commander",
        FleetRole::SquadMember => "Squad Member",
    }
}

pub(super) fn ensure_fleet_channel_member(
    chat_channels: &mut HashMap<String, ChatChannelRecord>,
    fleet_id: i64,
    boss_id: i64,
    char_id: i64,
) {
    let chan_key = format!("fleetid:{fleet_id}");
    let chan = chat_channels
        .entry(chan_key.clone())
        .or_insert_with(|| ChatChannelRecord {
            channel_id: chan_key,
            name: format!("Fleet {fleet_id}"),
            motd: String::new(),
            owner_id: Some(boss_id),
            members: HashSet::new(),
            is_system_local: false,
            solar_system_id: None,
            is_private: true,
            operators: HashSet::from([boss_id]),
            password: None,
            is_delayed_local: false,
        });
    chan.members.insert(char_id);
}

pub(super) async fn handle_fleets(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if !matches!(service, "fleetMgr" | "fleetObjectHandler" | "fleetProxy") {
        return None;
    }

    match (service, method) {
        ("fleetObjectHandler", "Init") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(fid) = session.fleet_id.or_else(|| notify::character_active_fleet_id(ctx, char_id)) else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
            };
            session.fleet_id = Some(fid);
            let ad_opt = kwargs.get("adInfoData").or_else(|| args.get(2)).filter(|v| !matches!(v, PyRep::None));
            let (created_unix, sys_id, reg_change) = ctx.db.with_mut_snapshot(|snap| {
                let sys = snap.characters.get(&char_id).map(|c| c.solar_system_id).or(session.solar_system_id).unwrap_or(0);
                let Some(f) = snap.fleets.get_mut(&fid) else { return (ctx.now_unix(), sys, None); };
                let created = f.member_details.values().map(|m| m.join_time_unix).min().unwrap_or_else(|| ctx.now_unix());
                let change = (ad_opt.is_some() && f.boss_id == char_id).then(|| { let old = f.is_registered; f.is_registered = true; (f.free_move, old) });
                (created, sys, change)
            });
            notify::emit_join(ctx, fid, char_id);
            if let Some((free_move, old_reg)) = reg_change {
                let ad_rep = ad_opt.unwrap();
                let fname = ad_rep.dict_get("fleetName").and_then(|v| v.as_str()).unwrap_or("Registered Fleet").to_string();
                let desc = ad_rep.dict_get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let scope = ad_rep.dict_get("inviteScope").and_then(|v| v.as_i64()).unwrap_or(3) as u8;
                let auto_join = ad_rep.dict_get("useAdvanceOptions").and_then(|v| v.as_bool()).unwrap_or(true);
                let min_st = ad_rep.dict_get("minStanding").and_then(|v| v.as_f64()).unwrap_or(-10.0);
                let min_sec = ad_rep.dict_get("minSecurity").and_then(|v| v.as_f64()).unwrap_or(-10.0);
                let auto = {
                    let mut ex = ctx.fleet_extras.lock().unwrap();
                    ex.fleet_names.insert(fid, fname.clone());
                    ex.adverts.insert(fid, FleetAdvertState {
                        fleet_id: fid, leader_id: char_id, fleet_name: fname, description: desc,
                        solar_system_id: sys_id, invite_scope: scope, auto_join, min_standing: min_st,
                        min_security: min_sec, allowed_entities: HashSet::new(), _created_unix: created_unix,
                    });
                    ex.auto_join_squads.get(&fid).copied().flatten()
                };
                notify::emit_options_changed(ctx, fid, notify::options_keyval(free_move, old_reg, auto), notify::options_keyval(free_move, true, auto));
            }
            Some(Ok(PyRep::None))
        }

        ("fleetMgr", "CreateFleet") | ("fleetObjectHandler", "CreateFleet") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            if notify::character_active_fleet_id(ctx, char_id).is_some() {
                return Some(Err(BridgeRpcError { code: "AlreadyInFleet".into(), message: "Character is already in a fleet".into() }));
            }
            let is_zero_arg_handler = service == "fleetObjectHandler" && args.is_empty();
            let fleet_name = args.first().and_then(|a| a.as_str()).unwrap_or("New Eden Taskforce").to_string();
            let motd = args.get(1).and_then(|a| a.as_str()).unwrap_or("Forming up — check MOTD").to_string();

            let (fleet_id, wing_id, squad_id) = ctx
                .db
                .try_with_mut_snapshot(|snap| -> Result<(i64, i64, i64), BridgeRpcError> {
                    snap.next_entity_id += 1;
                    let fid = snap.next_entity_id;
                    let (wid, sid) = if is_zero_arg_handler {
                        snap.next_entity_id += 1; let w = snap.next_entity_id;
                        snap.next_entity_id += 1; let s = snap.next_entity_id;
                        (w, s)
                    } else {
                        (1_i64, 1_i64)
                    };
                    let wing_details = HashMap::from([(wid, FleetWingRecord { wing_id: wid, name: "Wing 1".into(), commander_id: None, squad_ids: vec![sid] })]);
                    let squad_details = HashMap::from([(sid, FleetSquadRecord { squad_id: sid, wing_id: wid, name: "Squad 1".into(), commander_id: None })]);
                    let member_details = HashMap::from([(char_id, FleetMemberRecord {
                        character_id: char_id, role: FleetRole::FleetCommander, wing_id: None, squad_id: None,
                        takes_fleet_warp: true, accepts_regroup: true, accepts_conduit_jumps: true, job: 2,
                        role_name: "Fleet Commander".into(), join_time_unix: ctx.now_unix(),
                    })]);
                    snap.fleets.insert(fid, FleetRecord {
                        fleet_id: fid, boss_id: char_id, members: vec![char_id], wings: vec![wid], squads: vec![sid],
                        free_move: false, is_registered: false, wing_details, squad_details, member_details,
                        invites: HashSet::new(), motd: motd.clone(),
                    });
                    ensure_fleet_channel_member(&mut snap.chat_channels, fid, char_id, char_id);
                    if let Some(ch) = snap.characters.get_mut(&char_id) { ch.fleet_id = Some(fid); }
                    Ok((fid, wid, sid))
                })
                .unwrap();

            {
                let mut ex = ctx.fleet_extras.lock().unwrap();
                ex.fleet_names.insert(fleet_id, fleet_name.clone());
                ex.max_sizes.insert(fleet_id, 256);
                ex.auto_join_squads.insert(fleet_id, Some(squad_id));
            }

            session.fleet_id = Some(fleet_id);
            notify::emit_join(ctx, fleet_id, char_id);

            if is_zero_arg_handler {
                return Some(Ok(PyRep::Integer(fleet_id)));
            }
            Some(Ok(PyRep::keyval([
                ("fleetID", PyRep::Integer(fleet_id)),
                ("bossID", PyRep::Integer(char_id)),
                ("wingID", PyRep::Integer(wing_id)),
                ("squadID", PyRep::Integer(squad_id)),
                ("fleetName", PyRep::String(fleet_name)),
                ("motd", PyRep::String(motd)),
                ("channelName", PyRep::String(format!("fleetid:{fleet_id}"))),
            ])))
        }

        ("fleetMgr", "CreateWing") | ("fleetObjectHandler", "CreateWing") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
            };
            let wing_name = args.first().and_then(|a| a.as_str()).unwrap_or("New Wing");
            match crate::domain::fleets::create_fleet_wing(ctx, fleet_id, Some(char_id), Some(wing_name), true, 1, true) {
                Ok(created) => {
                    ctx.notify(NotificationTarget::Fleet(fleet_id), "OnFleetWingAdded", "fleetid", vec![PyRep::Integer(created.wing_id)]);
                    Some(Ok(PyRep::keyval([("fleetID", PyRep::Integer(fleet_id)), ("wingID", PyRep::Integer(created.wing_id)), ("name", PyRep::String(created.name))])))
                }
                Err(e) => Some(Err(crate::services::domain_err_to_rpc(e))),
            }
        }

        ("fleetObjectHandler", "DeleteWing") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
            };
            let Some(wing_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "DeleteWing requires wingID".into() }));
            };
            match crate::domain::fleets::delete_fleet_wing(ctx, fleet_id, Some(char_id), wing_id, true) {
                Ok(()) => {
                    ctx.notify(NotificationTarget::Fleet(fleet_id), "OnFleetWingDeleted", "fleetid", vec![PyRep::Integer(wing_id)]);
                    Some(Ok(PyRep::keyval([("wingID", PyRep::Integer(wing_id)), ("deleted", PyRep::Bool(true))])))
                }
                Err(e) => Some(Err(crate::services::domain_err_to_rpc(e))),
            }
        }

        ("fleetObjectHandler", "ChangeWingName") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
            };
            let Some(wing_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "ChangeWingName requires wingID".into() }));
            };
            let new_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("Renamed Wing");
            match crate::domain::fleets::rename_fleet_wing(ctx, fleet_id, Some(char_id), wing_id, new_name, false) {
                Ok(name) => {
                    ctx.notify(NotificationTarget::Fleet(fleet_id), "OnFleetWingNameChanged", "fleetid", vec![PyRep::Integer(wing_id), PyRep::String(name.clone())]);
                    Some(Ok(PyRep::keyval([("wingID", PyRep::Integer(wing_id)), ("name", PyRep::String(name))])))
                }
                Err(e) => Some(Err(crate::services::domain_err_to_rpc(e))),
            }
        }

        ("fleetMgr", "CreateSquad") | ("fleetObjectHandler", "CreateSquad") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
            };
            let wing_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let squad_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("New Squad");
            match crate::domain::fleets::create_fleet_squad(ctx, fleet_id, Some(char_id), wing_id, Some(squad_name), true, 1, true) {
                Ok(created) => {
                    ctx.notify(NotificationTarget::Fleet(fleet_id), "OnFleetSquadAdded", "fleetid", vec![PyRep::Integer(wing_id), PyRep::Integer(created.squad_id)]);
                    Some(Ok(PyRep::keyval([("fleetID", PyRep::Integer(fleet_id)), ("wingID", PyRep::Integer(wing_id)), ("squadID", PyRep::Integer(created.squad_id)), ("name", PyRep::String(created.name))])))
                }
                Err(e) => Some(Err(crate::services::domain_err_to_rpc(e))),
            }
        }

        ("fleetObjectHandler", "DeleteSquad") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
            };
            let Some(squad_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "DeleteSquad requires squadID".into() }));
            };
            match crate::domain::fleets::delete_fleet_squad(ctx, fleet_id, Some(char_id), squad_id, true) {
                Ok(()) => {
                    ctx.notify(NotificationTarget::Fleet(fleet_id), "OnFleetSquadDeleted", "fleetid", vec![PyRep::Integer(squad_id)]);
                    Some(Ok(PyRep::keyval([("squadID", PyRep::Integer(squad_id)), ("deleted", PyRep::Bool(true))])))
                }
                Err(e) => Some(Err(crate::services::domain_err_to_rpc(e))),
            }
        }

        ("fleetObjectHandler", "ChangeSquadName") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
            };
            let Some(squad_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "ChangeSquadName requires squadID".into() }));
            };
            let new_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("Renamed Squad");
            match crate::domain::fleets::rename_fleet_squad(ctx, fleet_id, Some(char_id), squad_id, new_name, false) {
                Ok(name) => {
                    ctx.notify(NotificationTarget::Fleet(fleet_id), "OnFleetSquadNameChanged", "fleetid", vec![PyRep::Integer(squad_id), PyRep::String(name.clone())]);
                    Some(Ok(PyRep::keyval([("squadID", PyRep::Integer(squad_id)), ("name", PyRep::String(name))])))
                }
                Err(e) => Some(Err(crate::services::domain_err_to_rpc(e))),
            }
        }

        ("fleetMgr", "Invite") | ("fleetObjectHandler", "Invite") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            notify::tick_fleet_lifecycle(ctx);
            let fleet_id = match session.fleet_id {
                Some(fid) => fid,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Character is not in a fleet".into(),
                    }))
                }
            };
            let Some(target_char_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Invite requires target characterID".into(),
                }));
            };
            let wing_id_opt = args.get(1).and_then(|a| a.as_i64());
            let squad_id_opt = args.get(2).and_then(|a| a.as_i64());
            let role = i64_to_role(args.get(3).and_then(|a| a.as_i64()).unwrap_or(4));
            let res = crate::domain::fleets::invite_fleet_member(
                ctx,
                crate::domain::fleets::InviteFleetMemberInput {
                    fleet_id,
                    caller_char_id: Some(char_id),
                    inviter_id_override: Some(char_id),
                    invitee_id: target_char_id,
                    role,
                    wing_id: wing_id_opt,
                    squad_id: squad_id_opt,
                    enforce_online_and_capacity_checks: true,
                    validate_hierarchy_strict: false,
                    now_unix: ctx.now_unix(),
                },
            );
            match res {
                Ok(out) => {
                    ctx.notify(
                        NotificationTarget::Character(out.invitee_id),
                        "OnFleetInvite",
                        "charid",
                        vec![
                            PyRep::Integer(out.fleet_id),
                            PyRep::Integer(out.inviter_id),
                            PyRep::Integer(out.wing_id),
                            PyRep::Integer(out.squad_id),
                            PyRep::Integer(out.role_num),
                        ],
                    );
                    Some(Ok(PyRep::keyval([
                        ("fleetID", PyRep::Integer(out.fleet_id)),
                        ("invitedCharacterID", PyRep::Integer(out.invitee_id)),
                        ("wingID", PyRep::Integer(out.wing_id)),
                        ("squadID", PyRep::Integer(out.squad_id)),
                    ])))
                }
                Err(e) => {
                    if e.code == "NotInFleet" || e.code == "FleetNotFound" {
                        session.fleet_id = None;
                    }
                    Some(Err(crate::services::domain_err_to_rpc(e)))
                }
            }
        }

        ("fleetMgr", "AcceptInvite") | ("fleetObjectHandler", "AcceptInvite") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            notify::tick_fleet_lifecycle(ctx);
            match crate::domain::fleets::accept_fleet_invite(
                ctx,
                crate::domain::fleets::AcceptFleetInviteInput {
                    char_id,
                    fleet_hint: args.first().and_then(|a| a.as_i64()),
                    now_unix: ctx.now_unix(),
                    current_solar_system_id: session.solar_system_id,
                },
            ) {
                Ok(out) => {
                    notify::sync_fleet_join_requests_to_db(ctx);
                    session.fleet_id = Some(out.fleet_id);
                    notify::emit_join(ctx, out.fleet_id, out.char_id);
                    crate::services::notify_emitters_character_services::emit_fleet_respawn_points_if_any(ctx, out.fleet_id);
                    if !out.motd.is_empty() {
                        notify::emit_motd(ctx, NotificationTarget::Character(out.char_id), &out.motd, false);
                    }
                    Some(Ok(PyRep::keyval([
                        ("fleetID", PyRep::Integer(out.fleet_id)),
                        ("characterID", PyRep::Integer(out.char_id)),
                        ("wingID", PyRep::Integer(out.wing_id)),
                        ("squadID", PyRep::Integer(out.squad_id)),
                        ("role", PyRep::Integer(role_to_i64(out.role))),
                        ("motd", PyRep::String(out.motd)),
                    ])))
                }
                Err(e) => Some(Err(crate::services::domain_err_to_rpc(e))),
            }
        }

        ("fleetObjectHandler", "RejectInvite") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            notify::tick_fleet_lifecycle(ctx);
            // Client: `GetFleet(fleetID).RejectInvite(isAlreadyInFleet)` (FleetSvc.OnFleetInvite).
            let already_in_fleet = matches!(args.first(), Some(PyRep::Bool(true)));
            let fleet_id = invited_fleet(ctx, char_id, args.first().and_then(|a| match a {
                PyRep::Integer(i) => Some(*i),
                _ => None,
            }));
            let inviter = {
                let mut ex = ctx.fleet_extras.lock().unwrap();
                ex.invite_timestamps.remove(&(fleet_id, char_id));
                ex.invite_details.remove(&(fleet_id, char_id)).map(|d| d.3)
            };
            let boss_opt = ctx
                .db
                .try_with_mut_snapshot(|snap| -> Result<Option<i64>, BridgeRpcError> {
                    if let Some(fleet) = snap.fleets.get_mut(&fleet_id) {
                        if fleet.invites.remove(&char_id) {
                            return Ok(Some(fleet.boss_id));
                        }
                    }
                    Ok(None)
                })
                .unwrap_or(None);
            if let Some(boss_id) = boss_opt {
                // FleetSvc.OnFleetJoinReject(memberID, reasonCode) to the inviter.
                let reason = if already_in_fleet {
                    PyRep::Integer(notify::REJECT_INVITE_ALREADY_IN_FLEET)
                } else {
                    PyRep::None
                };
                ctx.notify(
                    NotificationTarget::Character(inviter.unwrap_or(boss_id)),
                    "OnFleetJoinReject",
                    "charid",
                    vec![PyRep::Integer(char_id), reason],
                );
            }
            Some(Ok(PyRep::keyval([
                ("fleetID", PyRep::Integer(fleet_id)),
                ("rejected", PyRep::Bool(true)),
            ])))
        }

        ("fleetMgr", "KickMember")
        | ("fleetObjectHandler", "KickMember")
        | ("fleetMgr", "ForceLeaveFleet") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = match session.fleet_id {
                Some(fid) => fid,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Character is not in a fleet".into(),
                    }))
                }
            };
            let target_id = if method == "ForceLeaveFleet" {
                args.first().and_then(|a| a.as_i64()).unwrap_or(char_id)
            } else {
                let Some(tid) = args.first().and_then(|a| a.as_i64()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "KickMember requires target characterID".into(),
                    }));
                };
                tid
            };

            match crate::domain::fleets::kick_or_remove_fleet_member(
                ctx,
                fleet_id,
                Some(char_id),
                target_id,
                false,
            ) {
                Ok(outcome) => {
                    notify::emit_fleet_member_removed(ctx, &outcome);
                    if target_id == char_id {
                        session.fleet_id = None;
                    }
                    notify::emit_fleet_session_cleared(ctx, fleet_id, target_id, outcome.old_role_num);
                    Some(Ok(PyRep::keyval([
                        ("kickedCharacterID", PyRep::Integer(target_id)),
                        ("fleetID", PyRep::Integer(fleet_id)),
                        ("disbanded", PyRep::Bool(outcome.disbanded)),
                    ])))
                }
                Err(e) => {
                    if e.code == "NotInFleet" || e.code == "FleetNotFound" {
                        session.fleet_id = None;
                    }
                    Some(Err(crate::services::domain_err_to_rpc(e)))
                }
            }
        }

        ("fleetMgr", "LeaveFleet") | ("fleetObjectHandler", "LeaveFleet") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = match session.fleet_id {
                Some(fid) => fid,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Character is not in a fleet".into(),
                    }))
                }
            };
            let (disbanded, new_boss) = notify::remove_character_from_fleet(ctx, fleet_id, char_id);
            session.fleet_id = None;
            Some(Ok(PyRep::keyval([
                ("fleetID", PyRep::Integer(fleet_id)),
                ("leftFleetID", PyRep::Integer(fleet_id)),
                ("left", PyRep::Bool(true)),
                ("disbanded", PyRep::Bool(disbanded)),
                ("newBossID", new_boss.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ])))
        }

        ("fleetObjectHandler", "DisbandFleet") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = match session.fleet_id {
                Some(fid) => fid,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Character is not in a fleet".into(),
                    }))
                }
            };
            match crate::domain::fleets::disband_fleet(ctx, fleet_id, char_id) {
                Ok(out) => {
                    notify::sync_fleet_join_requests_to_db(ctx);
                    notify::sync_fleet_watchlist_to_db(ctx);
                    notify::sync_fleet_options_to_db(ctx);
                    session.fleet_id = None;
                    for mid in &out.former_member_ids {
                        notify::emit_leave(ctx, fleet_id, *mid);
                    }
                    let ids = PyRep::List(out.former_member_ids.iter().map(|&m| PyRep::Integer(m)).collect());
                    for mid in &out.former_member_ids {
                        ctx.notify(NotificationTarget::Character(*mid), "OnFleetDisbanded", "charid", vec![ids.clone()]);
                    }
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnMemberlessFleetUnregistered",
                        "charid",
                        vec![],
                    );
                    Some(Ok(PyRep::keyval([
                        ("fleetID", PyRep::Integer(fleet_id)),
                        ("disbanded", PyRep::Bool(true)),
                    ])))
                }
                Err(e) => Some(Err(crate::services::domain_err_to_rpc(e))),
            }
        }

        ("fleetObjectHandler", "FinishMove") if args.len() < 2 => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.fleet_id.is_none() && notify::character_active_fleet_id(ctx, char_id).is_none() {
                return Some(Err(BridgeRpcError {
                    code: "NotInFleet".into(),
                    message: "Character is not in a fleet".into(),
                }));
            }
            Some(Ok(PyRep::None))
        }

        ("fleetMgr", "MoveMember")
        | ("fleetObjectHandler", "MoveMember")
        | ("fleetObjectHandler", "FinishMove") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = match session.fleet_id {
                Some(fid) => fid,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Character is not in a fleet".into(),
                    }))
                }
            };
            let rpc = crate::router::RpcArgs::positional(args);
            let target_id = match rpc.opt_positive_i64(0, "charID") {
                Ok(v) => v.unwrap_or(char_id),
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            let new_wing_id = match rpc.opt_i64(1, "wingID") {
                Ok(v) => v.unwrap_or(1),
                Err(e) => return Some(Err(e)),
            };
            let new_squad_id = match rpc.opt_i64(2, "squadID") {
                Ok(v) => v.unwrap_or(1),
                Err(e) => return Some(Err(e)),
            };
            let new_role_num = match rpc.opt_i64(3, "role") {
                Ok(v) => v.unwrap_or(4),
                Err(e) => return Some(Err(e)),
            };
            let new_role = i64_to_role(new_role_num);

            match crate::domain::fleets::move_fleet_member(
                ctx,
                crate::domain::fleets::MoveFleetMemberInput {
                    fleet_id,
                    caller_char_id: Some(char_id),
                    target_id,
                    new_role,
                    wing_id: Some(new_wing_id),
                    squad_id: Some(new_squad_id),
                    strict_esi_wing_squad: false,
                    allow_fallback_missing_wing_squad: false,
                    enforce_session_change_timer: true,
                    now_unix,
                },
            ) {
                Ok(out) => {
                    notify::emit_member_changed(ctx, fleet_id, &out.before, &out.after, out.is_only_member);
                    ctx.notify(
                        NotificationTarget::Character(target_id),
                        "OnFleetMove",
                        "charid",
                        vec![],
                    );
                    Some(Ok(PyRep::keyval([
                        ("characterID", PyRep::Integer(target_id)),
                        ("wingID", PyRep::Integer(out.resolved_wing_id.unwrap_or(new_wing_id))),
                        ("squadID", PyRep::Integer(out.resolved_squad_id.unwrap_or(new_squad_id))),
                        ("role", PyRep::Integer(new_role_num)),
                    ])))
                }
                Err(e) => {
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnFleetMoveFailed",
                        "charid",
                        vec![PyRep::Integer(target_id), PyRep::Bool(false)],
                    );
                    Some(Err(crate::services::domain_err_to_rpc(e)))
                }
            }
        }

        _ => {
            super::fleet_social_fleet_ops::handle_fleet_ops(ctx, session, service, method, args, kwargs)
                .await
        }
    }
}

#[cfg(test)]
#[path = "fleet_social_fleets_tests.rs"]
mod fleet_social_fleets_tests;

