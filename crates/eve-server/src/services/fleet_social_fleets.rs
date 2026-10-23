//! Fleet Services (`SO-2`): `fleetMgr`, `fleetObjectHandler`, `fleetProxy`, and `beyonce.GetFormations`.

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{
    ChatChannelRecord, FleetMemberRecord, FleetRecord, FleetRole, FleetSquadRecord, FleetWingRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

use super::fleet_social_notify as notify;

#[derive(Clone, Debug)]
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
        ("fleetMgr", "CreateFleet")
        | ("fleetObjectHandler", "CreateFleet")
        | ("fleetObjectHandler", "Init") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if notify::character_active_fleet_id(ctx, char_id).is_some() {
                return Some(Err(BridgeRpcError {
                    code: "AlreadyInFleet".into(),
                    message: "Character is already in a fleet".into(),
                }));
            }
            let fleet_name = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("New Eden Taskforce")
                .to_string();
            let motd = args
                .get(1)
                .and_then(|a| a.as_str())
                .unwrap_or("Forming up — check MOTD")
                .to_string();

            let (fleet_id, wing_id, squad_id) = ctx
                .db
                .try_with_mut_snapshot(|snap| -> Result<(i64, i64, i64), BridgeRpcError> {
                    snap.next_entity_id += 1;
                    let fid = snap.next_entity_id;
                    let wid = 1_i64;
                    let sid = 1_i64;

                    let mut wing_details = HashMap::new();
                    wing_details.insert(
                        wid,
                        FleetWingRecord {
                            wing_id: wid,
                            name: "Wing 1".into(),
                            commander_id: None,
                            squad_ids: vec![sid],
                        },
                    );

                    let mut squad_details = HashMap::new();
                    squad_details.insert(
                        sid,
                        FleetSquadRecord {
                            squad_id: sid,
                            wing_id: wid,
                            name: "Squad 1".into(),
                            commander_id: Some(char_id),
                        },
                    );

                    let mut member_details = HashMap::new();
                    member_details.insert(
                        char_id,
                        FleetMemberRecord {
                            character_id: char_id,
                            role: FleetRole::FleetCommander,
                            wing_id: Some(wid),
                            squad_id: Some(sid),
                            takes_fleet_warp: true,
                            accepts_regroup: true,
                            accepts_conduit_jumps: true,
                            job: 2, // fleet boss bit
                            role_name: "Fleet Commander".into(),
                            join_time_unix: ctx.now_unix(),
                        },
                    );

                    snap.fleets.insert(
                        fid,
                        FleetRecord {
                            fleet_id: fid,
                            boss_id: char_id,
                            members: vec![char_id],
                            wings: vec![wid],
                            squads: vec![sid],
                            free_move: false,
                            is_registered: false,
                            wing_details,
                            squad_details,
                            member_details,
                            invites: HashSet::new(),
                            motd: motd.clone(),
                        },
                    );
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
            let wing_name = args.first().and_then(|a| a.as_str()).unwrap_or("New Wing");
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                let Some(caller_m) = fleet.member_details.get(&char_id) else {
                    return Err(BridgeRpcError { code: "NotInFleet".into(), message: "Caller is not in fleet".into() });
                };
                if char_id != fleet.boss_id && role_to_i64(caller_m.role) > 1 {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Only Fleet Boss or Fleet Commander can create wings".into(),
                    });
                }
                if fleet.wings.len() >= 5 {
                    return Err(BridgeRpcError {
                        code: "MaxWingsReached".into(),
                        message: "Fleet already has the maximum of 5 wings".into(),
                    });
                }
                let wid = (fleet.wings.iter().max().copied().unwrap_or(0) + 1).max(1);
                fleet.wings.push(wid);
                fleet.wing_details.insert(
                    wid,
                    FleetWingRecord {
                        wing_id: wid,
                        name: wing_name.to_string(),
                        commander_id: None,
                        squad_ids: Vec::new(),
                    },
                );
                Ok(wid)
            });

            match res {
                Ok(wid) => {
                    ctx.notify(
                        NotificationTarget::Fleet(fleet_id),
                        "OnFleetWingAdded",
                        "fleetid",
                        // FleetSvc.OnFleetWingAdded(wingID)
                        vec![PyRep::Integer(wid)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("fleetID", PyRep::Integer(fleet_id)),
                        ("wingID", PyRep::Integer(wid)),
                        ("name", PyRep::String(wing_name.to_string())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetObjectHandler", "DeleteWing") => {
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
            let Some(wing_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DeleteWing requires wingID".into(),
                }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                let Some(caller_m) = fleet.member_details.get(&char_id) else {
                    return Err(BridgeRpcError { code: "NotInFleet".into(), message: "Caller is not in fleet".into() });
                };
                if char_id != fleet.boss_id && role_to_i64(caller_m.role) > 1 {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Only Fleet Boss or Fleet Commander can delete wings".into(),
                    });
                }
                let has_members = fleet
                    .member_details
                    .values()
                    .any(|m| m.wing_id == Some(wing_id));
                if has_members {
                    return Err(BridgeRpcError {
                        code: "WingNotEmpty".into(),
                        message: "Cannot delete a wing that still contains members".into(),
                    });
                }
                let Some(removed_wing) = fleet.wing_details.remove(&wing_id) else {
                    return Err(BridgeRpcError {
                        code: "WingNotFound".into(),
                        message: format!("Wing {wing_id} not found"),
                    });
                };
                fleet.wings.retain(|&w| w != wing_id);
                for sid in removed_wing.squad_ids {
                    fleet.squads.retain(|&s| s != sid);
                    fleet.squad_details.remove(&sid);
                }
                Ok(())
            });
            match res {
                Ok(()) => {
                    ctx.notify(
                        NotificationTarget::Fleet(fleet_id),
                        "OnFleetWingDeleted",
                        "fleetid",
                        vec![PyRep::Integer(wing_id)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("wingID", PyRep::Integer(wing_id)),
                        ("deleted", PyRep::Bool(true)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetObjectHandler", "ChangeWingName") => {
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
            let Some(wing_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "ChangeWingName requires wingID".into(),
                }));
            };
            let new_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("Renamed Wing");
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                let Some(caller_m) = fleet.member_details.get(&char_id) else {
                    return Err(BridgeRpcError { code: "NotInFleet".into(), message: "Caller is not in fleet".into() });
                };
                if char_id != fleet.boss_id && role_to_i64(caller_m.role) > 2 {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Renaming a wing requires Wing/Fleet Commander or Boss".into(),
                    });
                }
                let Some(wing) = fleet.wing_details.get_mut(&wing_id) else {
                    return Err(BridgeRpcError {
                        code: "WingNotFound".into(),
                        message: format!("Wing {wing_id} not found"),
                    });
                };
                wing.name = new_name.to_string();
                Ok(())
            });
            match res {
                Ok(()) => {
                    ctx.notify(
                        NotificationTarget::Fleet(fleet_id),
                        "OnFleetWingNameChanged",
                        "fleetid",
                        vec![PyRep::Integer(wing_id), PyRep::String(new_name.to_string())],
                    );
                    Some(Ok(PyRep::keyval([
                        ("wingID", PyRep::Integer(wing_id)),
                        ("name", PyRep::String(new_name.to_string())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetMgr", "CreateSquad") | ("fleetObjectHandler", "CreateSquad") => {
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
            let wing_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let squad_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("New Squad");

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                let Some(caller_m) = fleet.member_details.get(&char_id) else {
                    return Err(BridgeRpcError { code: "NotInFleet".into(), message: "Caller is not in fleet".into() });
                };
                if char_id != fleet.boss_id && role_to_i64(caller_m.role) > 2 {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Only Fleet Boss, Fleet Commander, or Wing Commander can create squads".into(),
                    });
                }
                let Some(wing) = fleet.wing_details.get_mut(&wing_id) else {
                    return Err(BridgeRpcError {
                        code: "WingNotFound".into(),
                        message: format!("Wing {wing_id} not found in fleet {fleet_id}"),
                    });
                };
                if wing.squad_ids.len() >= 5 {
                    return Err(BridgeRpcError {
                        code: "MaxSquadsReached".into(),
                        message: "Wing already has the maximum of 5 squads".into(),
                    });
                }
                let sid = (fleet.squads.iter().max().copied().unwrap_or(0) + 1).max(1);
                wing.squad_ids.push(sid);
                fleet.squads.push(sid);
                fleet.squad_details.insert(
                    sid,
                    FleetSquadRecord {
                        squad_id: sid,
                        wing_id,
                        name: squad_name.to_string(),
                        commander_id: None,
                    },
                );
                Ok(sid)
            });

            match res {
                Ok(sid) => {
                    ctx.notify(
                        NotificationTarget::Fleet(fleet_id),
                        "OnFleetSquadAdded",
                        "fleetid",
                        // FleetSvc.OnFleetSquadAdded(wingID, squadID)
                        vec![PyRep::Integer(wing_id), PyRep::Integer(sid)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("fleetID", PyRep::Integer(fleet_id)),
                        ("wingID", PyRep::Integer(wing_id)),
                        ("squadID", PyRep::Integer(sid)),
                        ("name", PyRep::String(squad_name.to_string())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetObjectHandler", "DeleteSquad") => {
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
            let Some(squad_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DeleteSquad requires squadID".into(),
                }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                let Some(caller_m) = fleet.member_details.get(&char_id) else {
                    return Err(BridgeRpcError { code: "NotInFleet".into(), message: "Caller is not in fleet".into() });
                };
                if char_id != fleet.boss_id && role_to_i64(caller_m.role) > 2 {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Deleting a squad requires Wing/Fleet Commander or Boss".into(),
                    });
                }
                if fleet
                    .member_details
                    .values()
                    .any(|m| m.squad_id == Some(squad_id))
                {
                    return Err(BridgeRpcError {
                        code: "SquadNotEmpty".into(),
                        message: "Cannot delete a squad that still contains members".into(),
                    });
                }
                if fleet.squad_details.remove(&squad_id).is_none() {
                    return Err(BridgeRpcError {
                        code: "SquadNotFound".into(),
                        message: format!("Squad {squad_id} not found"),
                    });
                }
                fleet.squads.retain(|&s| s != squad_id);
                for wing in fleet.wing_details.values_mut() {
                    wing.squad_ids.retain(|&s| s != squad_id);
                }
                Ok(())
            });
            match res {
                Ok(()) => {
                    ctx.notify(
                        NotificationTarget::Fleet(fleet_id),
                        "OnFleetSquadDeleted",
                        "fleetid",
                        vec![PyRep::Integer(squad_id)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("squadID", PyRep::Integer(squad_id)),
                        ("deleted", PyRep::Bool(true)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetObjectHandler", "ChangeSquadName") => {
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
            let Some(squad_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "ChangeSquadName requires squadID".into(),
                }));
            };
            let new_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("Renamed Squad");
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                let Some(caller_m) = fleet.member_details.get(&char_id) else {
                    return Err(BridgeRpcError { code: "NotInFleet".into(), message: "Caller is not in fleet".into() });
                };
                if char_id != fleet.boss_id && role_to_i64(caller_m.role) > 3 {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Renaming a squad requires Squad/Wing/Fleet Commander or Boss".into(),
                    });
                }
                let Some(sq) = fleet.squad_details.get_mut(&squad_id) else {
                    return Err(BridgeRpcError {
                        code: "SquadNotFound".into(),
                        message: format!("Squad {squad_id} not found"),
                    });
                };
                sq.name = new_name.to_string();
                Ok(())
            });
            match res {
                Ok(()) => {
                    ctx.notify(
                        NotificationTarget::Fleet(fleet_id),
                        "OnFleetSquadNameChanged",
                        "fleetid",
                        vec![PyRep::Integer(squad_id), PyRep::String(new_name.to_string())],
                    );
                    Some(Ok(PyRep::keyval([
                        ("squadID", PyRep::Integer(squad_id)),
                        ("name", PyRep::String(new_name.to_string())),
                    ])))
                }
                Err(e) => Some(Err(e)),
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
            let (max_size, has_pending_ts) = {
                let ex = ctx.fleet_extras.lock().unwrap();
                (
                    ex.max_sizes.get(&fleet_id).copied().unwrap_or(256),
                    ex.invite_timestamps.keys().any(|&(_, cid)| cid == target_char_id),
                )
            };
            let target_online = notify::is_character_online_for_fleet(ctx, target_char_id);
            let target_in_fleet = notify::character_active_fleet_id(ctx, target_char_id).is_some();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let any_invite = snap.fleets.values().any(|f| f.invites.contains(&target_char_id));
                let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                let Some(caller_member) = fleet.member_details.get(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Caller is not in fleet".into(),
                    });
                };
                if char_id != fleet.boss_id && role_to_i64(caller_member.role) > 3 {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Only Fleet Boss or a Commander can invite members".into(),
                    });
                };
                if fleet.members.len() >= max_size as usize {
                    return Err(BridgeRpcError {
                        code: "FleetFull".into(),
                        message: format!("Fleet has reached its maximum size of {max_size}"),
                    });
                }
                if !target_online {
                    return Err(BridgeRpcError {
                        code: "CharacterOffline".into(),
                        message: "Cannot invite an offline character to a fleet".into(),
                    });
                }
                if target_in_fleet {
                    return Err(BridgeRpcError {
                        code: "AlreadyInFleet".into(),
                        message: "Target character is already in a fleet".into(),
                    });
                }
                if has_pending_ts || any_invite {
                    return Err(BridgeRpcError {
                        code: "InviteAlreadyPending".into(),
                        message: "Target character already has a pending fleet invitation".into(),
                    });
                }
                let default_wing = fleet.wings.first().copied().unwrap_or(1);
                let wid = wing_id_opt.unwrap_or(default_wing);
                let sid = squad_id_opt.unwrap_or_else(|| {
                    fleet
                        .wing_details
                        .get(&wid)
                        .and_then(|w| w.squad_ids.first().copied())
                        .or_else(|| fleet.squads.first().copied())
                        .unwrap_or(1)
                });
                fleet.invites.insert(target_char_id);
                Ok((wid, sid))
            });

            match res {
                Ok((wid, sid)) => {
                    {
                        let mut ex = ctx.fleet_extras.lock().unwrap();
                        ex.invite_details
                            .insert((fleet_id, target_char_id), (wid, sid, role, char_id));
                        ex.invite_timestamps
                            .insert((fleet_id, target_char_id), ctx.now_unix());
                    }
                    ctx.notify(
                        NotificationTarget::Character(target_char_id),
                        "OnFleetInvite",
                        "charid",
                        vec![
                            PyRep::Integer(fleet_id),
                            PyRep::Integer(char_id),
                            PyRep::Integer(wid),
                            PyRep::Integer(sid),
                            PyRep::Integer(role_to_i64(role)),
                        ],
                    );
                    Some(Ok(PyRep::keyval([
                        ("fleetID", PyRep::Integer(fleet_id)),
                        ("invitedCharacterID", PyRep::Integer(target_char_id)),
                        ("wingID", PyRep::Integer(wid)),
                        ("squadID", PyRep::Integer(sid)),
                    ])))
                }
                Err(e) => {
                    if e.code == "NotInFleet" || e.code == "FleetNotFound" {
                        session.fleet_id = None;
                    }
                    Some(Err(e))
                }
            }
        }

        ("fleetMgr", "AcceptInvite") | ("fleetObjectHandler", "AcceptInvite") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if notify::character_active_fleet_id(ctx, char_id).is_some() {
                return Some(Err(BridgeRpcError {
                    code: "AlreadyInFleet".into(),
                    message: "Character is already in a fleet".into(),
                }));
            }
            // FleetSvc.OnFleetInvite calls `GetFleet(fleetID).AcceptInvite(shipTypeID)` on the fleet
            // moniker, so the fleet is the one holding this character's pending invite.
            let fleet_id = invited_fleet(ctx, char_id, args.first().and_then(|a| a.as_i64()));
            if fleet_id == 0 {
                return Some(Err(BridgeRpcError {
                    code: "InviteExpired".into(),
                    message: "No active fleet invitation found or invitation expired".into(),
                }));
            }

            let (max_size, auto_squad, invite_detail) = {
                let ex = ctx.fleet_extras.lock().unwrap();
                let ms = ex.max_sizes.get(&fleet_id).copied().unwrap_or(256);
                let asq = ex.auto_join_squads.get(&fleet_id).copied().flatten();
                // Read only: the inviter's wing/squad/role choice survives a failed accept.
                let inv = ex.invite_details.get(&(fleet_id, char_id)).copied();
                (ms, asq, inv)
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                if fleet.members.len() >= max_size as usize {
                    return Err(BridgeRpcError {
                        code: "FleetFull".into(),
                        message: format!("Fleet has reached its maximum size of {max_size}"),
                    });
                }
                fleet.invites.remove(&char_id);
                let (wid, sid, role) = invite_detail.map(|(w, s, r, _)| (w, s, r)).unwrap_or_else(|| {
                    let w = fleet.wings.first().copied().unwrap_or(1);
                    let s = auto_squad
                        .or_else(|| {
                            fleet
                                .wing_details
                                .get(&w)
                                .and_then(|wing| wing.squad_ids.first().copied())
                        })
                        .or_else(|| fleet.squads.first().copied())
                        .unwrap_or(1);
                    (w, s, FleetRole::SquadMember)
                });

                if role == FleetRole::WingCommander {
                    if let Some(wing) = fleet.wing_details.get_mut(&wid) {
                        wing.commander_id = Some(char_id);
                    }
                }
                if let Some(sq) = fleet.squad_details.get_mut(&sid) {
                    if role == FleetRole::SquadCommander || sq.commander_id.is_none() {
                        sq.commander_id = Some(char_id);
                    }
                }

                if !fleet.members.contains(&char_id) {
                    fleet.members.push(char_id);
                }
                fleet.member_details.insert(
                    char_id,
                    FleetMemberRecord {
                        character_id: char_id,
                        role,
                        wing_id: Some(wid),
                        squad_id: Some(sid),
                        takes_fleet_warp: true,
                        accepts_regroup: true,
                        accepts_conduit_jumps: true,
                        job: 0,
                        role_name: role_name_str(role).into(),
                        join_time_unix: ctx.now_unix(),
                    },
                );
                let motd = fleet.motd.clone();
                let boss_id = fleet.boss_id;
                ensure_fleet_channel_member(&mut snap.chat_channels, fleet_id, boss_id, char_id);
                if let Some(ch) = snap.characters.get_mut(&char_id) { ch.fleet_id = Some(fleet_id); }
                Ok((wid, sid, role, motd))
            });

            match res {
                Ok((wid, sid, role, motd)) => {
                    {
                        let mut ex = ctx.fleet_extras.lock().unwrap();
                        ex.invite_details.remove(&(fleet_id, char_id));
                        ex.invite_timestamps.remove(&(fleet_id, char_id));
                    }
                    session.fleet_id = Some(fleet_id);
                    notify::emit_join(ctx, fleet_id, char_id);
                    if !motd.is_empty() {
                        notify::emit_motd(ctx, NotificationTarget::Character(char_id), &motd, false);
                    }
                    Some(Ok(PyRep::keyval([
                        ("fleetID", PyRep::Integer(fleet_id)),
                        ("characterID", PyRep::Integer(char_id)),
                        ("wingID", PyRep::Integer(wid)),
                        ("squadID", PyRep::Integer(sid)),
                        ("role", PyRep::Integer(role_to_i64(role))),
                        ("motd", PyRep::String(motd)),
                    ])))
                }
                Err(e) => Some(Err(e)),
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

            let check_res = ctx.db.with_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                let Some(caller_m) = fleet.member_details.get(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Caller is not in fleet".into(),
                    });
                };
                let caller_role = role_to_i64(caller_m.role);
                if target_id != char_id && char_id != fleet.boss_id && caller_role > 3 {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Only Fleet Boss or a Commander can kick fleet members".into(),
                    });
                }
                if target_id == fleet.boss_id && target_id != char_id {
                    return Err(BridgeRpcError {
                        code: "CannotKickBoss".into(),
                        message: "Cannot kick the fleet boss".into(),
                    });
                }
                let Some(target_m) = fleet.member_details.get(&target_id) else {
                    return Err(BridgeRpcError {
                        code: "MemberNotFound".into(),
                        message: format!("Character {target_id} is not in fleet {fleet_id}"),
                    });
                };
                Ok(role_to_i64(target_m.role))
            });

            match check_res {
                Ok(old_role) => {
                    let (disbanded, _) = notify::remove_character_from_fleet(ctx, fleet_id, target_id);
                    if target_id == char_id {
                        session.fleet_id = None;
                    }
                    let sess_change = PyRep::Dict(vec![
                        (
                            PyRep::String("fleetid".into()),
                            PyRep::Tuple(vec![PyRep::Integer(fleet_id), PyRep::None]),
                        ),
                        (
                            PyRep::String("fleetrole".into()),
                            PyRep::Tuple(vec![PyRep::Integer(old_role), PyRep::None]),
                        ),
                    ]);
                    ctx.notify(
                        NotificationTarget::Character(target_id),
                        "OnSessionChanged",
                        "charid",
                        vec![sess_change],
                    );
                    Some(Ok(PyRep::keyval([
                        ("kickedCharacterID", PyRep::Integer(target_id)),
                        ("fleetID", PyRep::Integer(fleet_id)),
                        ("disbanded", PyRep::Bool(disbanded)),
                    ])))
                }
                Err(e) => {
                    if e.code == "NotInFleet" || e.code == "FleetNotFound" {
                        session.fleet_id = None;
                    }
                    Some(Err(e))
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
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                if !fleet.member_details.contains_key(&char_id) {
                    return Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Caller is not in fleet".into(),
                    });
                }
                if fleet.boss_id != char_id {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Only the Fleet Boss can disband the fleet".into(),
                    });
                }
                let member_ids = fleet.members.clone();
                for mid in &member_ids {
                    if let Some(ch) = snap.characters.get_mut(mid) {
                        ch.fleet_id = None;
                    }
                }
                snap.fleets.remove(&fleet_id);
                snap.chat_channels.remove(&format!("fleetid:{fleet_id}"));
                Ok(member_ids)
            });
            match res {
                Ok(member_ids) => {
                    {
                        let mut ex = ctx.fleet_extras.lock().unwrap();
                        ex.adverts.remove(&fleet_id);
                        ex.fleet_names.remove(&fleet_id);
                        ex.max_sizes.remove(&fleet_id);
                        ex.auto_join_squads.remove(&fleet_id);
                        ex.invite_details.retain(|&(fid, _), _| fid != fleet_id);
                        ex.invite_timestamps.retain(|&(fid, _), _| fid != fleet_id);
                        ex.target_tags.remove(&fleet_id);
                        ex.watchlist.remove(&fleet_id);
                        for mid in &member_ids {
                            ex.disconnected_members.remove(mid);
                        }
                        ex.options.remove(&fleet_id);
                        ex.wing_squad_hierarchy.remove(&fleet_id);
                    }
                    session.fleet_id = None;
                    for mid in &member_ids {
                        notify::emit_leave(ctx, fleet_id, *mid);
                    }
                    let ids = PyRep::List(member_ids.iter().map(|&m| PyRep::Integer(m)).collect());
                    for mid in &member_ids {
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
                Err(e) => Some(Err(e)),
            }
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
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            let new_wing_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
            let new_squad_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(1);
            let new_role_num = args.get(3).and_then(|a| a.as_i64()).unwrap_or(4);
            let new_role = i64_to_role(new_role_num);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                let Some(caller_m) = fleet.member_details.get(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Caller is not in fleet".into(),
                    });
                };
                let caller_role = role_to_i64(caller_m.role);
                let self_free_move =
                    target_id == char_id && fleet.free_move && new_role == FleetRole::SquadMember;
                if !self_free_move && char_id != fleet.boss_id && caller_role > 2 {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Moving members requires commander privileges or fleet free-move".into(),
                    });
                }
                if new_role == FleetRole::FleetCommander
                    && fleet
                        .member_details
                        .values()
                        .any(|m| m.character_id != target_id && m.role == FleetRole::FleetCommander)
                {
                    return Err(BridgeRpcError {
                        code: "PositionOccupied".into(),
                        message: "Fleet Commander slot is already occupied".into(),
                    });
                }
                if new_role == FleetRole::WingCommander
                    && fleet.member_details.values().any(|m| {
                        m.character_id != target_id
                            && m.wing_id == Some(new_wing_id)
                            && m.role == FleetRole::WingCommander
                    })
                {
                    return Err(BridgeRpcError {
                        code: "PositionOccupied".into(),
                        message: "Wing Commander slot in target wing is already occupied".into(),
                    });
                }
                if new_role == FleetRole::SquadCommander
                    && fleet.member_details.values().any(|m| {
                        m.character_id != target_id
                            && m.squad_id == Some(new_squad_id)
                            && m.role == FleetRole::SquadCommander
                    })
                {
                    return Err(BridgeRpcError {
                        code: "PositionOccupied".into(),
                        message: "Squad Commander slot in target squad is already occupied".into(),
                    });
                }
                let Some(member) = fleet.member_details.get_mut(&target_id) else {
                    return Err(BridgeRpcError {
                        code: "MemberNotFound".into(),
                        message: format!("Character {target_id} is not in fleet {fleet_id}"),
                    });
                };
                let before = member.clone();
                let old_wing = member.wing_id;
                let old_squad = member.squad_id;
                member.wing_id = Some(new_wing_id);
                member.squad_id = Some(new_squad_id);
                member.role = new_role;
                member.role_name = role_name_str(new_role).into();
                let after = member.clone();
                let only = fleet.members.len() == 1;

                if let Some(ow) = old_wing {
                    if let Some(w) = fleet.wing_details.get_mut(&ow) {
                        if w.commander_id == Some(target_id) {
                            w.commander_id = None;
                        }
                    }
                }
                if let Some(os) = old_squad {
                    if let Some(s) = fleet.squad_details.get_mut(&os) {
                        if s.commander_id == Some(target_id) {
                            s.commander_id = None;
                        }
                    }
                }
                if new_role == FleetRole::WingCommander {
                    if let Some(w) = fleet.wing_details.get_mut(&new_wing_id) {
                        w.commander_id = Some(target_id);
                    }
                }
                if new_role == FleetRole::SquadCommander {
                    if let Some(s) = fleet.squad_details.get_mut(&new_squad_id) {
                        s.commander_id = Some(target_id);
                    }
                }
                Ok((before, after, only))
            });

            match res {
                Ok((before, after, only)) => {
                    notify::emit_member_changed(ctx, fleet_id, &before, &after, only);
                    ctx.notify(
                        NotificationTarget::Character(target_id),
                        "OnFleetMove",
                        "charid",
                        vec![],
                    );
                    Some(Ok(PyRep::keyval([
                        ("characterID", PyRep::Integer(target_id)),
                        ("wingID", PyRep::Integer(new_wing_id)),
                        ("squadID", PyRep::Integer(new_squad_id)),
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
                    Some(Err(e))
                }
            }
        }

        _ => {
            super::fleet_social_fleet_ops::handle_fleet_ops(ctx, session, service, method, args, kwargs)
                .await
        }
    }
}
