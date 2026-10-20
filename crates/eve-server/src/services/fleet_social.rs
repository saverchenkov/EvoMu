//! `fleet_social` — Domain 10: Fleets, Corporations, Chat (LSC), Mail, Bookmarks, and Probing/Wormholes (`SO-1`..`SO-7`).

#![forbid(unsafe_code)]

use crate::router::cache_notifications::NotificationTarget;
use crate::router::ServerContext;
use eve_db::{
    corp_roles, BookmarkFolderRecord, BookmarkRecord, ChatChannelRecord, CorpApplicationRecord,
    CorpWalletDivisionRecord, CorporationRecord, CosmicSignatureRecord,
    FleetMemberRecord, FleetRecord, FleetRole, FleetSquadRecord,
    FleetWingRecord, IskCents, MailLabelRecord, MailRecord,
};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::{HashMap, HashSet};

pub const AU_IN_METERS: f64 = 149_597_870_700.0;

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let method = if service == "fleetObjectHandler" {
        match method {
            "AcceptInvite" => "JoinFleet",
            "GetInitState" | "GetWings" => "GetFleet",
            "MoveMember" => "SetRole",
            "SendBroadcast" => "Broadcast",
            _ => method,
        }
    } else {
        method
    };
    match (service, method) {
        // =========================================================================
        // SO-1: Fleet Manager (`fleetMgr`, `fleetObjectHandler`, `fleetProxy`)
        // =========================================================================
        ("fleetMgr", "CreateFleet")
        | ("fleetObjectHandler", "CreateFleet")
        | ("fleetProxy", "CreateFleet")
        | ("fleetSvc", "InitFleet") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fid = ctx.db.try_with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;

                let mut wing_details = HashMap::new();
                wing_details.insert(
                    1,
                    FleetWingRecord {
                        wing_id: 1,
                        name: "Wing 1".into(),
                        commander_id: None,
                        squad_ids: vec![1],
                    },
                );

                let mut squad_details = HashMap::new();
                squad_details.insert(
                    1,
                    FleetSquadRecord {
                        squad_id: 1,
                        wing_id: 1,
                        name: "Squad 1".into(),
                        commander_id: None,
                    },
                );

                let mut member_details = HashMap::new();
                member_details.insert(
                    char_id,
                    FleetMemberRecord {
                        character_id: char_id,
                        role: FleetRole::FleetCommander,
                        wing_id: None,
                        squad_id: None,
                        takes_fleet_warp: true,
                        job: 0,
                        role_name: "Fleet Commander".into(),
                    },
                );

                snap.fleets.insert(
                    id,
                    FleetRecord {
                        fleet_id: id,
                        boss_id: char_id,
                        members: vec![char_id],
                        wings: vec![1],
                        squads: vec![1],
                        free_move: true,
                        is_registered: true,
                        wing_details,
                        squad_details,
                        member_details,
                        invites: HashSet::new(),
                        motd: "Fly safe and align on broadcast!".into(),
                    },
                );
                Ok::<_, std::convert::Infallible>(id)
            }).unwrap();

            session.fleet_id = Some(fid);
            Some(Ok(PyRep::keyval([
                ("fleetID", PyRep::Integer(fid)),
                ("bossID", PyRep::Integer(char_id)),
            ])))
        }

        ("fleetMgr", "CreateWing") | ("fleetObjectHandler", "CreateWing") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fid = match session.fleet_id {
                Some(f) => f,
                None => return Some(Err(BridgeRpcError {
                    code: "NotInFleet".into(),
                    message: "Character is not currently in a fleet".into(),
                })),
            };
            let wing_name = args.first().and_then(|a| a.as_str()).unwrap_or("New Wing");

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fl) = snap.fleets.get_mut(&fid) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fid} not found"),
                    });
                };
                if fl.boss_id != char_id {
                    let is_fc = fl.member_details.get(&char_id).is_some_and(|m| m.role == FleetRole::FleetCommander);
                    if !is_fc {
                        return Err(BridgeRpcError {
                            code: "FleetPermissionDenied".into(),
                            message: "Only the Fleet Commander or Boss can create wings".into(),
                        });
                    }
                }
                let wing_id = (fl.wings.iter().max().copied().unwrap_or(0) + 1).max(1);
                fl.wings.push(wing_id);
                fl.wing_details.insert(
                    wing_id,
                    FleetWingRecord {
                        wing_id,
                        name: wing_name.to_string(),
                        commander_id: None,
                        squad_ids: Vec::new(),
                    },
                );
                Ok(wing_id)
            });

            match res {
                Ok(wid) => Some(Ok(PyRep::keyval([
                    ("fleetID", PyRep::Integer(fid)),
                    ("wingID", PyRep::Integer(wid)),
                    ("name", PyRep::String(wing_name.to_string())),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetMgr", "CreateSquad") | ("fleetObjectHandler", "CreateSquad") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fid = match session.fleet_id {
                Some(f) => f,
                None => return Some(Err(BridgeRpcError {
                    code: "NotInFleet".into(),
                    message: "Character is not currently in a fleet".into(),
                })),
            };
            let wing_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let squad_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("New Squad");

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fl) = snap.fleets.get_mut(&fid) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fid} not found"),
                    });
                };
                let is_boss = fl.boss_id == char_id;
                let is_fc = fl.member_details.get(&char_id).is_some_and(|m| m.role == FleetRole::FleetCommander);
                let is_wc = fl.wing_details.get(&wing_id).and_then(|w| w.commander_id) == Some(char_id);
                if !is_boss && !is_fc && !is_wc {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Only Boss, FC, or Wing Commander can create squads".into(),
                    });
                }
                let squad_id = (fl.squads.iter().max().copied().unwrap_or(0) + 1).max(1);
                fl.squads.push(squad_id);
                fl.squad_details.insert(
                    squad_id,
                    FleetSquadRecord {
                        squad_id,
                        wing_id,
                        name: squad_name.to_string(),
                        commander_id: None,
                    },
                );
                if let Some(w) = fl.wing_details.get_mut(&wing_id) {
                    w.squad_ids.push(squad_id);
                }
                Ok(squad_id)
            });

            match res {
                Ok(sid) => Some(Ok(PyRep::keyval([
                    ("fleetID", PyRep::Integer(fid)),
                    ("wingID", PyRep::Integer(wing_id)),
                    ("squadID", PyRep::Integer(sid)),
                    ("name", PyRep::String(squad_name.to_string())),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetMgr", "Invite") | ("fleetMgr", "InviteMember") | ("fleetObjectHandler", "Invite") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fid = match session.fleet_id {
                Some(f) => f,
                None => return Some(Err(BridgeRpcError {
                    code: "NotInFleet".into(),
                    message: "Character is not currently in a fleet".into(),
                })),
            };
            let Some(candidate_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Invite requires candidate characterID".into(),
                }));
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fl) = snap.fleets.get_mut(&fid) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fid} not found"),
                    });
                };
                if fl.members.contains(&candidate_id) {
                    return Err(BridgeRpcError {
                        code: "AlreadyInFleet".into(),
                        message: format!("Character {candidate_id} is already in the fleet"),
                    });
                }
                fl.invites.insert(candidate_id);
                Ok(())
            });

            match res {
                Ok(()) => {
                    ctx.notify(
                        NotificationTarget::Character(candidate_id),
                        "OnFleetInvite",
                        "charid",
                        vec![PyRep::Integer(fid), PyRep::Integer(char_id)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("fleetID", PyRep::Integer(fid)),
                        ("invitedCharacterID", PyRep::Integer(candidate_id)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetMgr", "JoinFleet") | ("fleetMgr", "AcceptInvite") | ("fleetObjectHandler", "JoinFleet") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_fid = args.first().and_then(|a| a.as_i64()).or(session.fleet_id);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let fid = if let Some(fid) = target_fid {
                    fid
                } else {
                    // Find fleet with candidate in invites
                    snap.fleets.values().find(|f| f.invites.contains(&char_id)).map(|f| f.fleet_id).ok_or_else(|| BridgeRpcError {
                        code: "NoPendingInvite".into(),
                        message: "No pending fleet invite found for character".into(),
                    })?
                };

                let Some(fl) = snap.fleets.get_mut(&fid) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fid} not found"),
                    });
                };

                if !fl.free_move && !fl.invites.contains(&char_id) {
                    return Err(BridgeRpcError {
                        code: "InviteRequired".into(),
                        message: "Invitation required to join this fleet".into(),
                    });
                }

                fl.invites.remove(&char_id);
                if !fl.members.contains(&char_id) {
                    fl.members.push(char_id);
                }
                fl.member_details.insert(
                    char_id,
                    FleetMemberRecord {
                        character_id: char_id,
                        role: FleetRole::SquadMember,
                        wing_id: fl.wings.first().copied(),
                        squad_id: fl.squads.first().copied(),
                        takes_fleet_warp: true,
                        job: 0,
                        role_name: "Squad Member".into(),
                    },
                );
                Ok(fid)
            });

            match res {
                Ok(fid) => {
                    session.fleet_id = Some(fid);
                    ctx.notify(
                        NotificationTarget::Fleet(fid),
                        "OnFleetMemberJoined",
                        "fleetid",
                        vec![PyRep::Integer(fid), PyRep::Integer(char_id)],
                    );
                    ctx.notify(
                        NotificationTarget::Fleet(fid),
                        "OnFleetJoin",
                        "fleetid",
                        vec![PyRep::Integer(fid), PyRep::Integer(char_id)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("fleetID", PyRep::Integer(fid)),
                        ("characterID", PyRep::Integer(char_id)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetMgr", "KickMember") | ("fleetObjectHandler", "KickMember") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fid = match session.fleet_id {
                Some(f) => f,
                None => return Some(Err(BridgeRpcError {
                    code: "NotInFleet".into(),
                    message: "Character is not currently in a fleet".into(),
                })),
            };
            let Some(target_char_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "KickMember requires target characterID".into(),
                }));
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fl) = snap.fleets.get_mut(&fid) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fid} not found"),
                    });
                };
                let is_boss = fl.boss_id == char_id;
                let is_fc = fl.member_details.get(&char_id).is_some_and(|m| m.role == FleetRole::FleetCommander);
                if !is_boss && !is_fc {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Only Boss or Fleet Commander can kick members".into(),
                    });
                }
                if target_char_id == fl.boss_id {
                    return Err(BridgeRpcError {
                        code: "CannotKickBoss".into(),
                        message: "Cannot kick the fleet boss".into(),
                    });
                }
                fl.members.retain(|&m| m != target_char_id);
                fl.member_details.remove(&target_char_id);
                Ok(())
            });

            match res {
                Ok(()) => {
                    ctx.notify(
                        NotificationTarget::Character(target_char_id),
                        "OnFleetMemberKicked",
                        "charid",
                        vec![PyRep::Integer(fid)],
                    );
                    ctx.notify(
                        NotificationTarget::Character(target_char_id),
                        "OnFleetLeave",
                        "charid",
                        vec![PyRep::Integer(fid), PyRep::Integer(target_char_id)],
                    );
                    ctx.notify(
                        NotificationTarget::Fleet(fid),
                        "OnFleetMemberLeft",
                        "fleetid",
                        vec![PyRep::Integer(fid), PyRep::Integer(target_char_id)],
                    );
                    ctx.notify(
                        NotificationTarget::Fleet(fid),
                        "OnFleetLeave",
                        "fleetid",
                        vec![PyRep::Integer(fid), PyRep::Integer(target_char_id)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("fleetID", PyRep::Integer(fid)),
                        ("kickedCharacterID", PyRep::Integer(target_char_id)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetMgr", "LeaveFleet") | ("fleetMgr", "ForceLeaveFleet") | ("fleetObjectHandler", "LeaveFleet") | ("fleetObjectHandler", "ForceLeaveFleet") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fid = match session.fleet_id {
                Some(f) => f,
                None => return Some(Err(BridgeRpcError {
                    code: "NotInFleet".into(),
                    message: "Character is not currently in a fleet".into(),
                })),
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fl) = snap.fleets.get_mut(&fid) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fid} not found"),
                    });
                };
                fl.members.retain(|&m| m != char_id);
                fl.member_details.remove(&char_id);

                if fl.members.is_empty() {
                    snap.fleets.remove(&fid);
                    return Ok(true);
                }
                if fl.boss_id == char_id {
                    let next_boss = fl.members[0];
                    fl.boss_id = next_boss;
                    if let Some(m) = fl.member_details.get_mut(&next_boss) {
                        m.role = FleetRole::FleetCommander;
                        m.role_name = "Fleet Commander".into();
                    }
                }
                Ok(false)
            });

            match res {
                Ok(dissolved) => {
                    session.fleet_id = None;
                    if !dissolved {
                        ctx.notify(
                            NotificationTarget::Fleet(fid),
                            "OnFleetMemberLeft",
                            "fleetid",
                            vec![PyRep::Integer(fid), PyRep::Integer(char_id)],
                        );
                        ctx.notify(
                            NotificationTarget::Fleet(fid),
                            "OnFleetLeave",
                            "fleetid",
                            vec![PyRep::Integer(fid), PyRep::Integer(char_id)],
                        );
                    }
                    Some(Ok(PyRep::keyval([
                        ("fleetID", PyRep::Integer(fid)),
                        ("left", PyRep::Bool(true)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetMgr", "SetRole") | ("fleetMgr", "MoveMember") | ("fleetObjectHandler", "SetRole") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fid = match session.fleet_id {
                Some(f) => f,
                None => return Some(Err(BridgeRpcError {
                    code: "NotInFleet".into(),
                    message: "Character is not currently in a fleet".into(),
                })),
            };
            let Some(target_char_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "SetRole requires target characterID".into(),
                }));
            };
            let role_num = args.get(1).and_then(|a| a.as_i64()).unwrap_or(4);
            let role = match role_num {
                1 => FleetRole::FleetCommander,
                2 => FleetRole::WingCommander,
                3 => FleetRole::SquadCommander,
                _ => FleetRole::SquadMember,
            };
            let wing_id = args.get(2).and_then(|a| a.as_i64());
            let squad_id = args.get(3).and_then(|a| a.as_i64());

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fl) = snap.fleets.get_mut(&fid) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fid} not found"),
                    });
                };
                let is_boss = fl.boss_id == char_id;
                let is_fc = fl.member_details.get(&char_id).is_some_and(|m| m.role == FleetRole::FleetCommander);
                if !is_boss && !is_fc {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Only Boss or Fleet Commander can modify member roles".into(),
                    });
                }
                let Some(member) = fl.member_details.get_mut(&target_char_id) else {
                    return Err(BridgeRpcError {
                        code: "MemberNotInFleet".into(),
                        message: format!("Character {target_char_id} is not in the fleet"),
                    });
                };
                member.role = role;
                member.wing_id = wing_id;
                member.squad_id = squad_id;
                member.role_name = match role {
                    FleetRole::FleetCommander => "Fleet Commander".into(),
                    FleetRole::WingCommander => "Wing Commander".into(),
                    FleetRole::SquadCommander => "Squad Commander".into(),
                    FleetRole::SquadMember => "Squad Member".into(),
                };

                if role == FleetRole::WingCommander {
                    if let Some(wid) = wing_id {
                        if let Some(w) = fl.wing_details.get_mut(&wid) {
                            w.commander_id = Some(target_char_id);
                        }
                    }
                } else if role == FleetRole::SquadCommander {
                    if let Some(sid) = squad_id {
                        if let Some(s) = fl.squad_details.get_mut(&sid) {
                            s.commander_id = Some(target_char_id);
                        }
                    }
                }
                Ok(())
            });

            match res {
                Ok(()) => {
                    ctx.notify(
                        NotificationTarget::Fleet(fid),
                        "OnFleetMemberChanged",
                        "fleetid",
                        vec![
                            PyRep::Integer(fid),
                            PyRep::Integer(target_char_id),
                            PyRep::Integer(role_num),
                        ],
                    );
                    Some(Ok(PyRep::keyval([
                        ("characterID", PyRep::Integer(target_char_id)),
                        ("role", PyRep::Integer(role_num)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetMgr", "WarpToMember") | ("fleetMgr", "CmdWarpToMember") | ("fleetObjectHandler", "WarpToMember") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fid = match session.fleet_id {
                Some(f) => f,
                None => return Some(Err(BridgeRpcError {
                    code: "NotInFleet".into(),
                    message: "Character is not currently in a fleet".into(),
                })),
            };
            let Some(target_char_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "WarpToMember requires targetCharacterID".into(),
                }));
            };
            let warp_dist = args.get(1).and_then(|a| a.as_f64()).unwrap_or(0.0);
            let Some(caller_sys) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NotInSpace".into(),
                    message: "Caller is not in a solar system".into(),
                }));
            };
            let Some(caller_ship) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Caller has no active ship".into(),
                }));
            };

            let target_ship_opt = ctx.db.with_snapshot(|snap| {
                let fl = snap.fleets.get(&fid)?;
                if !fl.members.contains(&target_char_id) {
                    return None;
                }
                let target_ch = snap.characters.get(&target_char_id)?;
                if target_ch.solar_system_id != caller_sys || target_ch.station_id.is_some() || target_ch.structure_id.is_some() {
                    return None;
                }
                Some(target_ch.active_ship_id)
            });

            let Some(target_ship_id) = target_ship_opt else {
                return Some(Err(BridgeRpcError {
                    code: "TargetUnavailable".into(),
                    message: "Target fleet member is not in space in the same solar system".into(),
                }));
            };

            let sys_handle = ctx.systems.get_or_spawn(caller_sys);
            let target_ball = sys_handle.get_ball_snapshot(target_ship_id).await.ok().flatten();
            let Some((_, t_ball)) = target_ball else {
                return Some(Err(BridgeRpcError {
                    code: "TargetNotInSpace".into(),
                    message: "Target ship ball is not present in solar system".into(),
                }));
            };

            let dest = t_ball.position + DVec3::new(warp_dist, 0.0, 0.0);
            if let Ok(Some((_, c_ball))) = sys_handle.get_ball_snapshot(caller_ship).await {
                let warp_mode = MovementMode::new_warp(c_ball.position, dest, 3.0);
                let _ = sys_handle.set_movement_mode(caller_ship, warp_mode).await;
            }

            Some(Ok(PyRep::keyval([
                ("status", PyRep::String("Warping".into())),
                ("targetCharacterID", PyRep::Integer(target_char_id)),
                ("destinationX", PyRep::Real(dest.x)),
                ("destinationY", PyRep::Real(dest.y)),
                ("destinationZ", PyRep::Real(dest.z)),
                ("distance", PyRep::Real(warp_dist)),
            ])))
        }

        ("fleetMgr", "CmdFleetWarp") | ("fleetMgr", "FleetWarp") | ("fleetObjectHandler", "CmdFleetWarp") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fid = match session.fleet_id {
                Some(f) => f,
                None => return Some(Err(BridgeRpcError {
                    code: "NotInFleet".into(),
                    message: "Character is not currently in a fleet".into(),
                })),
            };
            let Some(caller_sys) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NotInSpace".into(),
                    message: "Caller is not in space".into(),
                }));
            };

            let (dest, warp_dist) = if args.len() >= 3 {
                let x = args.get(0).and_then(|a| a.as_f64()).unwrap_or(0.0);
                let y = args.get(1).and_then(|a| a.as_f64()).unwrap_or(0.0);
                let z = args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.0);
                let d = args.get(3).and_then(|a| a.as_f64()).unwrap_or(0.0);
                (DVec3::new(x, y, z), d)
            } else {
                let ent_id = args.get(0).and_then(|a| a.as_i64()).unwrap_or(0);
                let d = args.get(1).and_then(|a| a.as_f64()).unwrap_or(0.0);
                let sys_handle = ctx.systems.get_or_spawn(caller_sys);
                let pos = if let Ok(Some((_, b))) = sys_handle.get_ball_snapshot(ent_id).await {
                    b.position
                } else if let Some(st) = ctx.sde.solar_systems.get(&caller_sys).and_then(|s| s.stations.iter().find(|st| st.station_id == ent_id)) {
                    DVec3::new(st.position.x, st.position.y, st.position.z)
                } else {
                    DVec3::new(100_000_000.0, 0.0, 0.0)
                };
                (pos, d)
            };

            let members_to_warp = ctx.db.with_snapshot(|snap| {
                let fl = snap.fleets.get(&fid)?;
                let caller_mem = fl.member_details.get(&char_id)?;
                if caller_mem.role == FleetRole::SquadMember {
                    return None;
                }
                let mut ships = Vec::new();
                for &mid in &fl.members {
                    if let Some(mem) = fl.member_details.get(&mid) {
                        if mem.takes_fleet_warp {
                            if let Some(ch) = snap.characters.get(&mid) {
                                if ch.solar_system_id == caller_sys && ch.station_id.is_none() && ch.structure_id.is_none() {
                                    ships.push((mid, ch.active_ship_id));
                                }
                            }
                        }
                    }
                }
                Some(ships)
            });

            let Some(ships_list) = members_to_warp else {
                return Some(Err(BridgeRpcError {
                    code: "FleetWarpDenied".into(),
                    message: "Only fleet commanders can initiate fleet warp".into(),
                }));
            };

            let sys_handle = ctx.systems.get_or_spawn(caller_sys);
            let warp_target = dest + DVec3::new(warp_dist, 0.0, 0.0);
            for &(_mid, ship_id) in &ships_list {
                if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(ship_id).await {
                    let warp_mode = MovementMode::new_warp(ball.position, warp_target, 3.0);
                    let _ = sys_handle.set_movement_mode(ship_id, warp_mode).await;
                }
            }

            ctx.notify(
                NotificationTarget::Fleet(fid),
                "OnFleetBroadcast",
                "fleetid",
                vec![
                    PyRep::String("FleetWarp".into()),
                    PyRep::Integer(char_id),
                    PyRep::Real(warp_target.x),
                    PyRep::Real(warp_target.y),
                    PyRep::Real(warp_target.z),
                ],
            );

            Some(Ok(PyRep::keyval([
                ("status", PyRep::String("FleetWarpInitiated".into())),
                ("warpCount", PyRep::Integer(ships_list.len() as i64)),
                ("destinationX", PyRep::Real(warp_target.x)),
                ("destinationY", PyRep::Real(warp_target.y)),
                ("destinationZ", PyRep::Real(warp_target.z)),
            ])))
        }

        ("fleetMgr", "Broadcast") | ("fleetMgr", "SendBroadcast") | ("fleetObjectHandler", "Broadcast") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fid = match session.fleet_id {
                Some(f) => f,
                None => return Some(Err(BridgeRpcError {
                    code: "NotInFleet".into(),
                    message: "Character is not currently in a fleet".into(),
                })),
            };
            let bcast_type = args.first().and_then(|a| a.as_str()).unwrap_or("WarpTo");
            let target_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let item_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
            let sys_id = session.solar_system_id.unwrap_or(0);
            let now_ft = ctx.now_blueos_filetime_ticks();

            let res = ctx.db.with_snapshot(|snap| {
                snap.fleets.contains_key(&fid)
            });
            if !res {
                return Some(Err(BridgeRpcError {
                    code: "FleetNotFound".into(),
                    message: format!("Fleet {fid} not found"),
                }));
            }

            ctx.notify(
                NotificationTarget::Fleet(fid),
                "OnFleetBroadcast",
                "fleetid",
                vec![
                    PyRep::String(bcast_type.to_string()),
                    PyRep::Integer(char_id),
                    PyRep::Integer(target_id),
                    PyRep::Integer(sys_id as i64),
                    PyRep::Integer(item_id),
                    PyRep::Integer(now_ft),
                ],
            );

            Some(Ok(PyRep::keyval([
                ("broadcastType", PyRep::String(bcast_type.to_string())),
                ("senderID", PyRep::Integer(char_id)),
                ("targetID", PyRep::Integer(target_id)),
            ])))
        }

        ("fleetMgr", "GetMemberPositions") | ("fleetMgr", "GetFleetPositions") | ("fleetObjectHandler", "GetMemberPositions") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fid = match session.fleet_id {
                Some(f) => f,
                None => return Some(Err(BridgeRpcError {
                    code: "NotInFleet".into(),
                    message: "Character is not currently in a fleet".into(),
                })),
            };

            let members_info = ctx.db.with_snapshot(|snap| {
                let Some(fl) = snap.fleets.get(&fid) else { return None; };
                let mut res = Vec::new();
                for &mid in &fl.members {
                    if let Some(ch) = snap.characters.get(&mid) {
                        let ship_type = snap.items.get(&ch.active_ship_id).map(|i| i.type_id).unwrap_or(0);
                        res.push((mid, ch.solar_system_id, ch.active_ship_id, ship_type, ch.station_id, ch.structure_id));
                    }
                }
                Some(res)
            });

            let Some(info_list) = members_info else {
                return Some(Err(BridgeRpcError {
                    code: "FleetNotFound".into(),
                    message: format!("Fleet {fid} not found"),
                }));
            };

            let mut rows = Vec::new();
            for (mid, sys_id, ship_id, ship_type, st_id, struct_id) in info_list {
                let mut pos = (0.0, 0.0, 0.0);
                if let Some(station_id) = st_id {
                    if let Some(st) = ctx.sde.solar_systems.get(&sys_id).and_then(|s| s.stations.iter().find(|st| st.station_id == station_id)) {
                        pos = (st.position.x, st.position.y, st.position.z);
                    }
                } else if let Some(structure_id) = struct_id {
                    let spos = ctx.db.with_snapshot(|snap| snap.structures.get(&structure_id).map(|s| (s.position[0], s.position[1], s.position[2])));
                    if let Some(p) = spos { pos = p; }
                } else {
                    let sys_handle = ctx.systems.get_or_spawn(sys_id);
                    if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(ship_id).await {
                        pos = (ball.position.x, ball.position.y, ball.position.z);
                    }
                }

                rows.push(vec![
                    PyRep::Integer(mid),
                    PyRep::Integer(sys_id as i64),
                    PyRep::Real(pos.0),
                    PyRep::Real(pos.1),
                    PyRep::Real(pos.2),
                    PyRep::Integer(ship_type as i64),
                ]);
            }

            Some(Ok(PyRep::rowset(
                vec!["characterID", "solarSystemID", "x", "y", "z", "shipTypeID"],
                rows,
            )))
        }

        ("fleetMgr", "GetFleet") | ("fleetMgr", "GetInitState") | ("fleetObjectHandler", "GetFleet") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fid = match session.fleet_id {
                Some(f) => f,
                None => return Some(Err(BridgeRpcError {
                    code: "NotInFleet".into(),
                    message: "Character is not currently in a fleet".into(),
                })),
            };

            let fleet_data = ctx.db.with_snapshot(|snap| {
                let fl = snap.fleets.get(&fid)?;
                if !fl.members.contains(&char_id) {
                    return None;
                }
                let members_rows: Vec<Vec<PyRep>> = fl.member_details.values().map(|m| {
                    vec![
                        PyRep::Integer(m.character_id),
                        PyRep::Integer(m.role as u8 as i64),
                        m.wing_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                        m.squad_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                        PyRep::Bool(m.takes_fleet_warp),
                        PyRep::String(m.role_name.clone()),
                    ]
                }).collect();

                let wings_rows: Vec<Vec<PyRep>> = fl.wing_details.values().map(|w| {
                    vec![
                        PyRep::Integer(w.wing_id),
                        PyRep::String(w.name.clone()),
                        w.commander_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    ]
                }).collect();

                let squads_rows: Vec<Vec<PyRep>> = fl.squad_details.values().map(|s| {
                    vec![
                        PyRep::Integer(s.squad_id),
                        PyRep::Integer(s.wing_id),
                        PyRep::String(s.name.clone()),
                        s.commander_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    ]
                }).collect();

                Some((fl.boss_id, fl.motd.clone(), members_rows, wings_rows, squads_rows))
            });

            let Some((boss_id, motd, m_rows, w_rows, s_rows)) = fleet_data else {
                session.fleet_id = None;
                return Some(Err(BridgeRpcError {
                    code: "NotInFleet".into(),
                    message: format!("Character {char_id} is no longer in fleet {fid}"),
                }));
            };

            Some(Ok(PyRep::keyval([
                ("fleetID", PyRep::Integer(fid)),
                ("bossID", PyRep::Integer(boss_id)),
                ("motd", PyRep::String(motd)),
                ("members", PyRep::rowset(vec!["characterID", "role", "wingID", "squadID", "takesFleetWarp", "roleName"], m_rows)),
                ("wings", PyRep::rowset(vec!["wingID", "name", "commanderID"], w_rows)),
                ("squads", PyRep::rowset(vec!["squadID", "wingID", "name", "commanderID"], s_rows)),
            ])))
        }

        // =========================================================================
        // SO-2: Corporation & Alliance Management
        // =========================================================================
        ("corpmgr", "CreateCorporation") | ("corpRegistry", "AddCorporation") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let name = args.first().and_then(|a| a.as_str()).unwrap_or("EvoMu Industrial Corp");
            let ticker = args.get(1).and_then(|a| a.as_str()).unwrap_or("EVOMU");
            let tax_rate = args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.10).clamp(0.0, 1.0);
            let reg_fee = IskCents::from_isk(1_599_800);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let corp_mgmt_lvl = snap
                    .skills
                    .get(&char_id)
                    .and_then(|m| m.get(&3363))
                    .map(|s| s.level)
                    .unwrap_or(0);
                if corp_mgmt_lvl < 1 {
                    return Err(BridgeRpcError {
                        code: "MissingCorporationManagementSkill".into(),
                        message: "Creating a corporation requires Corporation Management (3363) level 1".into(),
                    });
                }
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let new_bal = ch.balance.checked_sub(reg_fee).ok_or_else(|| BridgeRpcError {
                    code: "InsufficientFunds".into(),
                    message: "Not enough ISK to pay corporation registration fee (1,599,800 ISK)".into(),
                })?;
                ch.balance = new_bal;

                snap.next_entity_id += 1;
                let cid = 98_000_000 + snap.next_entity_id;
                if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
                    ch_mut.corporation_id = cid;
                    ch_mut.alliance_id = None;
                }

                let wallet_divisions: Vec<CorpWalletDivisionRecord> = (1..=7)
                    .map(|d| CorpWalletDivisionRecord {
                        division_id: d,
                        name: if d == 1 { "Master Wallet".into() } else { format!("Division {d}") },
                        balance: IskCents(0),
                    })
                    .collect();

                let mut member_roles = HashMap::new();
                member_roles.insert(char_id, u64::MAX);

                snap.corporations.insert(
                    cid,
                    CorporationRecord {
                        corporation_id: cid,
                        name: name.to_string(),
                        ticker: ticker.to_string(),
                        ceo_id: char_id,
                        alliance_id: None,
                        tax_rate,
                        master_wallet: IskCents(0),
                        members: HashSet::from([char_id]),
                        member_roles,
                        applications: HashMap::new(),
                        wallet_divisions,
                        description: String::new(),
                        url: String::new(),
                        hq_station_id: Some(60_003_760),
                    },
                );
                Ok(cid)
            });

            match res {
                Ok(corp_id) => {
                    session.corporation_id = Some(corp_id);
                    session.alliance_id = None;
                    session.corp_role = u64::MAX;
                    Some(Ok(PyRep::keyval([
                        ("corporationID", PyRep::Integer(corp_id)),
                        ("allianceID", PyRep::None),
                        ("name", PyRep::String(name.to_string())),
                        ("ticker", PyRep::String(ticker.to_string())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("corpmgr", "GetMembers") | ("corpmgr", "GetMemberTracking") | ("corporationSvc", "GetMembers") => {
            let (_, _) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = args.first().and_then(|a| a.as_i64()).or(session.corporation_id).unwrap_or(0);

            let rows = ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else { return Vec::new(); };
                corp.members.iter().filter_map(|&mid| {
                    snap.characters.get(&mid).map(|ch| {
                        let role_mask = corp.member_roles.get(&mid).copied().unwrap_or(0);
                        let title = if mid == corp.ceo_id { "CEO" } else { "Member" };
                        vec![
                            PyRep::Integer(ch.character_id),
                            PyRep::String(ch.name.clone()),
                            PyRep::Integer(role_mask as i64),
                            PyRep::String(title.into()),
                            PyRep::Integer(corp_id),
                        ]
                    })
                }).collect()
            });

            Some(Ok(PyRep::rowset(
                vec!["characterID", "name", "role", "title", "corporationID"],
                rows,
            )))
        }

        ("corpmgr", "UpdateMemberRoles") | ("corporationSvc", "UpdateMemberRoles") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let db_corp_id = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .filter(|cid| snap.corporations.contains_key(cid))
            });
            if let Some(cid) = db_corp_id {
                session.corporation_id = Some(cid);
            }
            let corp_id = match session.corporation_id {
                Some(c) => c,
                None => return Some(Err(BridgeRpcError {
                    code: "NoCorporation".into(),
                    message: "Character has no active corporation".into(),
                })),
            };
            let Some(target_char_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "UpdateMemberRoles requires target characterID".into(),
                }));
            };
            let new_roles = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0) as u64;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if char_id != corp.ceo_id && !corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR) {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Only CEO or Director can update corporation roles".into(),
                    });
                }
                if !corp.members.contains(&target_char_id) {
                    return Err(BridgeRpcError {
                        code: "TargetNotInCorp".into(),
                        message: format!("Character {target_char_id} is not a member of corporation {corp_id}"),
                    });
                }
                corp.member_roles.insert(target_char_id, new_roles);
                Ok(())
            });

            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("characterID", PyRep::Integer(target_char_id)),
                    ("roles", PyRep::Integer(new_roles as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("corpmgr", "ApplyForMembership") | ("corpmgr", "CreateApplication") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(corp_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "ApplyForMembership requires target corporationID".into(),
                }));
            };
            let message = args.get(1).and_then(|a| a.as_str()).unwrap_or("o7 requesting to join");
            let now_unix = ctx.now_unix();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                if corp.members.contains(&char_id) {
                    return Err(BridgeRpcError {
                        code: "AlreadyInCorporation".into(),
                        message: "Character is already a member of this corporation".into(),
                    });
                }
                snap.next_entity_id += 1;
                let app_id = snap.next_entity_id;
                corp.applications.insert(
                    app_id,
                    CorpApplicationRecord {
                        application_id: app_id,
                        character_id: char_id,
                        corporation_id: corp_id,
                        message: message.to_string(),
                        created_unix: now_unix,
                        status: "Applied".into(),
                    },
                );
                Ok(app_id)
            });

            match res {
                Ok(app_id) => {
                    ctx.notify(
                        NotificationTarget::Corporation(corp_id),
                        "OnCorpApplicationReceived",
                        "corpid",
                        vec![PyRep::Integer(corp_id), PyRep::Integer(app_id), PyRep::Integer(char_id)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("applicationID", PyRep::Integer(app_id)),
                        ("status", PyRep::String("Applied".into())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("corpmgr", "GetApplications") | ("corporationSvc", "GetApplications") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = args.first().and_then(|a| a.as_i64()).or(session.corporation_id).unwrap_or(0);

            let res = ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if char_id != corp.ceo_id && !corp_roles::has_role(caller_role, corp_roles::ROLE_PERSONNEL_MANAGER) {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Viewing applications requires Personnel Manager or Director role".into(),
                    });
                }
                let rows: Vec<Vec<PyRep>> = corp.applications.values().map(|app| {
                    vec![
                        PyRep::Integer(app.application_id),
                        PyRep::Integer(app.character_id),
                        PyRep::String(app.message.clone()),
                        PyRep::String(app.status.clone()),
                        PyRep::Integer(app.created_unix as i64),
                    ]
                }).collect();
                Ok(rows)
            });

            match res {
                Ok(rows) => Some(Ok(PyRep::rowset(
                    vec!["applicationID", "characterID", "message", "status", "createdDate"],
                    rows,
                ))),
                Err(e) => Some(Err(e)),
            }
        }

        ("corpmgr", "AcceptApplication") | ("corporationSvc", "AcceptApplication") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = match session.corporation_id {
                Some(c) => c,
                None => return Some(Err(BridgeRpcError {
                    code: "NoCorporation".into(),
                    message: "Character has no active corporation".into(),
                })),
            };
            let Some(app_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "AcceptApplication requires applicationID".into(),
                }));
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if char_id != corp.ceo_id && !corp_roles::has_role(caller_role, corp_roles::ROLE_PERSONNEL_MANAGER) {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Accepting applications requires Personnel Manager or Director role".into(),
                    });
                }
                let Some(app) = corp.applications.get_mut(&app_id) else {
                    return Err(BridgeRpcError {
                        code: "ApplicationNotFound".into(),
                        message: format!("Application {app_id} not found"),
                    });
                };
                let applicant_id = app.character_id;
                app.status = "Accepted".into();

                corp.members.insert(applicant_id);
                corp.member_roles.insert(applicant_id, 0);

                if let Some(ch) = snap.characters.get_mut(&applicant_id) {
                    ch.corporation_id = corp_id;
                    ch.alliance_id = corp.alliance_id;
                }
                Ok(applicant_id)
            });

            match res {
                Ok(applicant_id) => {
                    ctx.notify(
                        NotificationTarget::Character(applicant_id),
                        "OnCorpApplicationAccepted",
                        "charid",
                        vec![PyRep::Integer(corp_id), PyRep::Integer(app_id)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("applicationID", PyRep::Integer(app_id)),
                        ("status", PyRep::String("Accepted".into())),
                        ("characterID", PyRep::Integer(applicant_id)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("corpmgr", "RejectApplication") | ("corporationSvc", "RejectApplication") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = match session.corporation_id {
                Some(c) => c,
                None => return Some(Err(BridgeRpcError {
                    code: "NoCorporation".into(),
                    message: "Character has no active corporation".into(),
                })),
            };
            let Some(app_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "RejectApplication requires applicationID".into(),
                }));
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if char_id != corp.ceo_id && !corp_roles::has_role(caller_role, corp_roles::ROLE_PERSONNEL_MANAGER) {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Rejecting applications requires Personnel Manager or Director role".into(),
                    });
                }
                let Some(app) = corp.applications.get_mut(&app_id) else {
                    return Err(BridgeRpcError {
                        code: "ApplicationNotFound".into(),
                        message: format!("Application {app_id} not found"),
                    });
                };
                let applicant_id = app.character_id;
                app.status = "Rejected".into();
                Ok(applicant_id)
            });

            match res {
                Ok(applicant_id) => {
                    ctx.notify(
                        NotificationTarget::Character(applicant_id),
                        "OnCorpApplicationRejected",
                        "charid",
                        vec![PyRep::Integer(corp_id), PyRep::Integer(app_id)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("applicationID", PyRep::Integer(app_id)),
                        ("status", PyRep::String("Rejected".into())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("corpmgr", "GetWalletDivisions") | ("corporationSvc", "GetWalletDivisions") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = args.first().and_then(|a| a.as_i64()).or(session.corporation_id).unwrap_or(0);

            let res = ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if char_id != corp.ceo_id
                    && !corp_roles::has_role(caller_role, corp_roles::ROLE_ACCOUNTANT)
                    && !corp_roles::has_role(caller_role, corp_roles::ROLE_JUNIOR_ACCOUNTANT)
                {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Viewing corporation wallet divisions requires Accountant role".into(),
                    });
                }
                let rows: Vec<Vec<PyRep>> = (1..=7u8)
                    .map(|d| {
                        let name = corp
                            .wallet_divisions
                            .iter()
                            .find(|w| w.division_id == d)
                            .map(|w| w.name.clone())
                            .unwrap_or_else(|| {
                                if d == 1 {
                                    "Master Wallet".to_string()
                                } else {
                                    format!("Division {d}")
                                }
                            });
                        vec![
                            PyRep::Integer(d as i64),
                            PyRep::String(name),
                            PyRep::Real(corp.division_balance(d).to_wire_f64()),
                        ]
                    })
                    .collect();
                Ok(rows)
            });

            match res {
                Ok(rows) => Some(Ok(PyRep::rowset(
                    vec!["divisionID", "name", "balance"],
                    rows,
                ))),
                Err(e) => Some(Err(e)),
            }
        }

        ("corpmgr", "TransferWalletDivision") | ("corporationSvc", "TransferWalletDivision") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = match session.corporation_id {
                Some(c) => c,
                None => return Some(Err(BridgeRpcError {
                    code: "NoCorporation".into(),
                    message: "Character has no active corporation".into(),
                })),
            };
            let from_div = args.first().and_then(|a| a.as_i64()).unwrap_or(1) as u8;
            let to_div = args.get(1).and_then(|a| a.as_i64()).unwrap_or(2) as u8;
            let amount_isk = args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.0);
            if amount_isk <= 0.0 || !amount_isk.is_finite() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Transfer amount must be positive and finite".into(),
                }));
            }
            let transfer_amount = IskCents::from_f64_isk(amount_isk);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if char_id != corp.ceo_id && !corp_roles::has_role(caller_role, corp_roles::ROLE_ACCOUNTANT) {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Transferring between corp wallet divisions requires Accountant role".into(),
                    });
                }
                if !(1..=7).contains(&from_div) {
                    return Err(BridgeRpcError {
                        code: "DivisionNotFound".into(),
                        message: format!("Source wallet division {from_div} not found"),
                    });
                }
                if !(1..=7).contains(&to_div) {
                    return Err(BridgeRpcError {
                        code: "DivisionNotFound".into(),
                        message: format!("Target wallet division {to_div} not found"),
                    });
                }
                corp.ensure_wallet_divisions();
                let from_pos = corp.wallet_divisions.iter().position(|d| d.division_id == from_div).ok_or_else(|| BridgeRpcError {
                    code: "DivisionNotFound".into(),
                    message: format!("Source wallet division {from_div} not found"),
                })?;
                let to_pos = corp.wallet_divisions.iter().position(|d| d.division_id == to_div).ok_or_else(|| BridgeRpcError {
                    code: "DivisionNotFound".into(),
                    message: format!("Target wallet division {to_div} not found"),
                })?;

                let source_bal = corp.wallet_divisions[from_pos].balance;
                let new_source = source_bal.checked_sub(transfer_amount).ok_or_else(|| BridgeRpcError {
                    code: "InsufficientDivisionFunds".into(),
                    message: "Insufficient funds in source wallet division".into(),
                })?;
                let new_target = corp.wallet_divisions[to_pos].balance.checked_add(transfer_amount).unwrap_or(corp.wallet_divisions[to_pos].balance);

                corp.wallet_divisions[from_pos].balance = new_source;
                corp.wallet_divisions[to_pos].balance = new_target;
                if from_div == 1 {
                    corp.master_wallet = new_source;
                }
                if to_div == 1 {
                    corp.master_wallet = new_target;
                }
                Ok(())
            });

            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("fromDivisionID", PyRep::Integer(from_div as i64)),
                    ("toDivisionID", PyRep::Integer(to_div as i64)),
                    ("transferredISK", PyRep::Real(amount_isk)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("corpmgr", "UpdateDivisionNames") | ("corpRegistry", "UpdateDivisionNames") | ("corporationSvc", "UpdateDivisionNames") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = match session.corporation_id {
                Some(c) => c,
                None => return Some(Err(BridgeRpcError {
                    code: "NoCorporation".into(),
                    message: "Character has no active corporation".into(),
                })),
            };
            let div_id = args.get(0).and_then(|a| a.as_i64()).unwrap_or(1) as u8;
            let new_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("Division");

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if char_id != corp.ceo_id && !corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR) {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Only CEO or Director can update division names".into(),
                    });
                }
                if let Some(div) = corp.wallet_divisions.iter_mut().find(|d| d.division_id == div_id) {
                    div.name = new_name.to_string();
                } else {
                    return Err(BridgeRpcError {
                        code: "DivisionNotFound".into(),
                        message: format!("Division {div_id} not found"),
                    });
                }
                Ok(())
            });

            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("divisionID", PyRep::Integer(div_id as i64)),
                    ("name", PyRep::String(new_name.to_string())),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("corpmgr", "GetPublicInfo") | ("corporationSvc", "GetCorpInfo") | ("corpmgr", "GetCorporationInfo") => {
            let (_, _) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = args.first().and_then(|a| a.as_i64()).or(session.corporation_id).unwrap_or(0);
            let corp_info = ctx.db.with_snapshot(|snap| {
                snap.corporations.get(&corp_id).map(|c| {
                    (
                        c.corporation_id,
                        c.name.clone(),
                        c.ticker.clone(),
                        c.ceo_id,
                        c.members.len() as i64,
                        c.tax_rate,
                        c.alliance_id,
                        c.hq_station_id.unwrap_or(60_003_760),
                        c.description.clone(),
                        c.url.clone(),
                    )
                })
            });

            let Some((cid, name, ticker, ceo_id, m_count, tax, alliance_id, hq_station, desc, url)) = corp_info else {
                return Some(Err(BridgeRpcError {
                    code: "CorporationNotFound".into(),
                    message: format!("Corporation {corp_id} not found"),
                }));
            };

            Some(Ok(PyRep::keyval([
                ("corporationID", PyRep::Integer(cid)),
                ("corporationName", PyRep::String(name)),
                ("ticker", PyRep::String(ticker)),
                ("ceoID", PyRep::Integer(ceo_id)),
                ("memberCount", PyRep::Integer(m_count)),
                ("taxRate", PyRep::Real(tax)),
                ("allianceID", alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("stationID", PyRep::Integer(hq_station)),
                ("description", PyRep::String(desc)),
                ("url", PyRep::String(url)),
            ])))
        }

        ("corpmgr", "UpdateCorporationDetails") | ("corpmgr", "UpdateCorpInfo") | ("corpRegistry", "UpdateCorporationDetails") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = match session.corporation_id {
                Some(c) => c,
                None => return Some(Err(BridgeRpcError {
                    code: "NoCorporation".into(),
                    message: "Character has no active corporation".into(),
                })),
            };
            let (desc, url, tax_rate_opt) = if args.first().and_then(|a| a.as_i64()).is_some() {
                (
                    args.get(1).and_then(|a| a.as_str()).unwrap_or(""),
                    args.get(2).and_then(|a| a.as_str()).unwrap_or(""),
                    args.get(3).and_then(|a| a.as_f64()),
                )
            } else {
                (
                    args.first().and_then(|a| a.as_str()).unwrap_or(""),
                    args.get(1).and_then(|a| a.as_str()).unwrap_or(""),
                    args.get(2).and_then(|a| a.as_f64()),
                )
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if char_id != corp.ceo_id && !corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR) {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Only CEO or Director can update corporation details".into(),
                    });
                }
                corp.description = desc.to_string();
                corp.url = url.to_string();
                if let Some(t) = tax_rate_opt {
                    corp.tax_rate = t.clamp(0.0, 1.0);
                }
                Ok((corp.description.clone(), corp.url.clone(), corp.tax_rate))
            });

            match res {
                Ok((desc, url, tax)) => {
                    ctx.notify(
                        NotificationTarget::All,
                        "OnCorporationChanged",
                        "corpid",
                        vec![PyRep::Integer(corp_id)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("corporationID", PyRep::Integer(corp_id)),
                        ("description", PyRep::String(desc)),
                        ("url", PyRep::String(url)),
                        ("taxRate", PyRep::Real(tax)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        // =========================================================================
        // SO-3: Chat System (`lscService`, `lsc`, `chat`, `chatMgr`)
        // =========================================================================
        ("lscService", "JoinChannel") | ("lsc", "JoinChannel") | ("chat", "JoinChannel") | ("chatMgr", "JoinChannel") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(chan_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "JoinChannel requires channelID string".into(),
                }));
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let chan = snap.chat_channels.entry(chan_id.to_string()).or_insert_with(|| {
                    ChatChannelRecord {
                        channel_id: chan_id.to_string(),
                        name: chan_id.to_string(),
                        motd: String::new(),
                        owner_id: Some(char_id),
                        members: HashSet::new(),
                        is_system_local: chan_id.starts_with("solarsystemid2:"),
                        solar_system_id: None,
                        is_private: chan_id.starts_with("private:"),
                        operators: HashSet::from([char_id]),
                        password: None,
                        is_delayed_local: chan_id.starts_with("solarsystemid2:31"),
                    }
                });
                chan.members.insert(char_id);
                Ok((chan.name.clone(), chan.members.len()))
            });

            match res {
                Ok((name, count)) => {
                    ctx.notify(
                        NotificationTarget::All,
                        "OnLSC",
                        "channelid",
                        vec![
                            PyRep::String(chan_id.to_string()),
                            PyRep::String("Join".into()),
                            PyRep::Integer(char_id),
                        ],
                    );
                    Some(Ok(PyRep::keyval([
                        ("channelID", PyRep::String(chan_id.to_string())),
                        ("name", PyRep::String(name)),
                        ("memberCount", PyRep::Integer(count as i64)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("lscService", "LeaveChannel") | ("lsc", "LeaveChannel") | ("chat", "LeaveChannel") | ("chatMgr", "LeaveChannel") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(chan_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "LeaveChannel requires channelID string".into(),
                }));
            };

            let _ = ctx.db.try_with_mut_snapshot(|snap| {
                if let Some(chan) = snap.chat_channels.get_mut(chan_id) {
                    chan.members.remove(&char_id);
                }
                Ok::<(), std::convert::Infallible>(())
            });

            ctx.notify(
                NotificationTarget::All,
                "OnLSC",
                "channelid",
                vec![
                    PyRep::String(chan_id.to_string()),
                    PyRep::String("Leave".into()),
                    PyRep::Integer(char_id),
                ],
            );

            Some(Ok(PyRep::keyval([
                ("channelID", PyRep::String(chan_id.to_string())),
                ("left", PyRep::Bool(true)),
            ])))
        }

        ("lscService", "SendMessage") | ("lsc", "SendMessage") | ("chat", "SendMessage") | ("chatMgr", "SendMessage") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(chan_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "SendMessage requires channelID string".into(),
                }));
            };
            let text = args.get(1).and_then(|a| a.as_str()).unwrap_or("o7");
            let now_ft = ctx.now_blueos_filetime_ticks();

            let sender_name = ctx.db.with_snapshot(|snap| {
                snap.characters.get(&char_id).map(|c| c.name.clone()).unwrap_or_else(|| "Pilot".into())
            });

            // Target dispatch based on channel type
            let notif_args = vec![
                PyRep::String(chan_id.to_string()),
                PyRep::Integer(char_id),
                PyRep::String(sender_name),
                PyRep::String(text.to_string()),
                PyRep::Integer(now_ft),
            ];

            if chan_id.starts_with("solarsystemid2:") {
                let sys_id: u32 = chan_id.split(':').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                ctx.notify(NotificationTarget::SolarSystem(sys_id), "OnLSC", "channelid", notif_args);
            } else if chan_id.starts_with("corpid:") {
                let corp_id: i64 = chan_id.split(':').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                ctx.notify(NotificationTarget::Corporation(corp_id), "OnLSC", "channelid", notif_args);
            } else if chan_id.starts_with("allianceid:") {
                let alliance_id: i64 = chan_id.split(':').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                ctx.notify(NotificationTarget::Alliance(alliance_id), "OnLSC", "channelid", notif_args);
            } else if chan_id.starts_with("fleetid:") {
                let fleet_id: i64 = chan_id.split(':').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                ctx.notify(NotificationTarget::Fleet(fleet_id), "OnLSC", "channelid", notif_args);
            } else {
                // General or private channel -> deliver to channel members
                let members: Vec<i64> = ctx.db.with_snapshot(|snap| {
                    snap.chat_channels.get(chan_id).map(|c| c.members.iter().copied().collect()).unwrap_or_default()
                });
                for mid in members {
                    ctx.notify(NotificationTarget::Character(mid), "OnLSC", "channelid", notif_args.clone());
                }
            }

            Some(Ok(PyRep::keyval([
                ("channelID", PyRep::String(chan_id.to_string())),
                ("sent", PyRep::Bool(true)),
            ])))
        }

        ("lscService", "StartConversation") | ("lsc", "SendPrivateMessage") | ("chat", "StartConversation") | ("chatMgr", "StartConversation") | ("chatMgr", "CreatePrivateChannel") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(recipient_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Private conversation requires recipient characterID".into(),
                }));
            };
            let text = args.get(1).and_then(|a| a.as_str()).unwrap_or("o7 pilot");
            let c1 = char_id.min(recipient_id);
            let c2 = char_id.max(recipient_id);
            let chan_id = format!("private:{c1}:{c2}");
            let now_ft = ctx.now_blueos_filetime_ticks();

            let sender_name = ctx.db.with_snapshot(|snap| {
                snap.characters.get(&char_id).map(|c| c.name.clone()).unwrap_or_else(|| "Pilot".into())
            });

            let notif_args = vec![
                PyRep::String(chan_id.clone()),
                PyRep::Integer(char_id),
                PyRep::String(sender_name),
                PyRep::String(text.to_string()),
                PyRep::Integer(now_ft),
            ];

            ctx.notify(NotificationTarget::Character(char_id), "OnLSC", "channelid", notif_args.clone());
            ctx.notify(NotificationTarget::Character(recipient_id), "OnLSC", "channelid", notif_args);

            Some(Ok(PyRep::keyval([
                ("channelID", PyRep::String(chan_id)),
                ("recipientID", PyRep::Integer(recipient_id)),
                ("sent", PyRep::Bool(true)),
            ])))
        }

        ("lscService", "GetChannelInfo") | ("lsc", "GetChannelMembers") | ("chatMgr", "GetChannelInfo") => {
            let (_, _) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(chan_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetChannelInfo requires channelID".into(),
                }));
            };

            let chan_opt = ctx.db.with_snapshot(|snap| snap.chat_channels.get(chan_id).cloned());
            let Some(chan) = chan_opt else {
                return Some(Err(BridgeRpcError {
                    code: "ChannelNotFound".into(),
                    message: format!("Channel {chan_id} does not exist"),
                }));
            };

            let rows: Vec<Vec<PyRep>> = chan.members.iter().map(|&m| {
                vec![PyRep::Integer(m), PyRep::String(chan_id.to_string())]
            }).collect();

            Some(Ok(PyRep::keyval([
                ("channelID", PyRep::String(chan.channel_id)),
                ("name", PyRep::String(chan.name)),
                ("motd", PyRep::String(chan.motd)),
                ("ownerID", chan.owner_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("members", PyRep::rowset(vec!["characterID", "channelID"], rows)),
            ])))
        }

        ("chatMgr", "CreateChannel") | ("chatMgr", "CreatePlayerOwnedChannel") | ("lscService", "CreatePlayerOwnedChannel") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let name = args.first().and_then(|a| a.as_str()).unwrap_or("PlayerChannel");
            let password = args.get(1).and_then(|a| a.as_str()).map(|s| s.to_string());
            let motd = args.get(2).and_then(|a| a.as_str()).unwrap_or("");
            let chan_id = format!("channel:{}", name.to_lowercase().replace(' ', "_"));

            ctx.db.try_with_mut_snapshot(|snap| {
                snap.chat_channels.insert(
                    chan_id.clone(),
                    ChatChannelRecord {
                        channel_id: chan_id.clone(),
                        name: name.to_string(),
                        motd: motd.to_string(),
                        owner_id: Some(char_id),
                        members: HashSet::from([char_id]),
                        is_system_local: false,
                        solar_system_id: None,
                        is_private: false,
                        operators: HashSet::from([char_id]),
                        password,
                        is_delayed_local: false,
                    },
                );
                Ok::<(), std::convert::Infallible>(())
            }).unwrap();

            Some(Ok(PyRep::keyval([
                ("channelID", PyRep::String(chan_id)),
                ("name", PyRep::String(name.to_string())),
                ("ownerID", PyRep::Integer(char_id)),
            ])))
        }

        ("chatMgr", "GrantChannelOwnership") | ("lscService", "GrantChannelOwnership") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(chan_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GrantChannelOwnership requires channelID".into(),
                }));
            };
            let Some(new_owner_id) = args.get(1).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GrantChannelOwnership requires newOwnerID".into(),
                }));
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let chan_key = if snap.chat_channels.contains_key(chan_id) {
                    chan_id.to_string()
                } else {
                    chan_id.to_lowercase()
                };
                let Some(chan) = snap.chat_channels.get_mut(&chan_key) else {
                    return Err(BridgeRpcError {
                        code: "ChannelNotFound".into(),
                        message: format!("Channel {chan_id} not found"),
                    });
                };
                if chan.owner_id != Some(char_id) {
                    return Err(BridgeRpcError {
                        code: "NotChannelOwner".into(),
                        message: "Only the channel owner can transfer ownership".into(),
                    });
                }
                chan.owner_id = Some(new_owner_id);
                chan.operators.insert(new_owner_id);
                Ok(())
            });

            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("channelID", PyRep::String(chan_id.to_string())),
                    ("ownerID", PyRep::Integer(new_owner_id)),
                    ("transferred", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        // =========================================================================
        // SO-4: In-Game Mail (`mailMgr`)
        // =========================================================================
        ("mailMgr", "SendMail") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut recipient_ids: Vec<i64> = Vec::new();
            match args.first() {
                Some(PyRep::List(list)) | Some(PyRep::Tuple(list)) => {
                    for item in list {
                        if let Some(id) = item.as_i64() {
                            recipient_ids.push(id);
                        }
                    }
                }
                Some(rep) => {
                    if let Some(id) = rep.as_i64() {
                        recipient_ids.push(id);
                    }
                }
                None => {}
            }
            let (subject, body) = if args.len() >= 5 {
                if recipient_ids.is_empty() {
                    if let Some(corp_or_alliance_id) = args.get(2).and_then(|a| a.as_i64()) {
                        let corp_members = ctx.db.with_snapshot(|snap| {
                            snap.corporations
                                .get(&corp_or_alliance_id)
                                .map(|c| c.members.iter().copied().collect::<Vec<_>>())
                                .unwrap_or_default()
                        });
                        recipient_ids.extend(corp_members);
                    }
                }
                (
                    args.get(3).and_then(|a| a.as_str()).unwrap_or("o7"),
                    args.get(4).and_then(|a| a.as_str()).unwrap_or("Fly safe!"),
                )
            } else {
                (
                    args.get(1).and_then(|a| a.as_str()).unwrap_or("o7"),
                    args.get(2).and_then(|a| a.as_str()).unwrap_or("Fly safe!"),
                )
            };
            if recipient_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidRecipient".into(),
                    message: "SendMail requires recipientID".into(),
                }));
            }
            let recipient_id = recipient_ids[0];

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if snap.mails.len() >= 1_000 {
                    return Err(BridgeRpcError {
                        code: "MailQuotaExceeded".into(),
                        message: "Server mail spool limit (1,000 messages) reached".into(),
                    });
                }
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                for &rec_id in &recipient_ids {
                    snap.mails.push(MailRecord {
                        message_id: id,
                        sender_id: char_id,
                        recipient_id: rec_id,
                        subject: subject.to_string(),
                        body: body.to_string(),
                    });
                    // Default label assignments: 1 (Inbox) for recipient, 2 (Sent) for sender
                    snap.mail_assigned_labels.insert((rec_id, id), vec![1]);
                }
                snap.mail_assigned_labels.insert((char_id, id), vec![2]);
                // Sender automatically marks their sent mail as read
                snap.read_mails.insert((char_id, id));
                Ok(id)
            });

            match res {
                Ok(mid) => {
                    for &rec_id in &recipient_ids {
                        ctx.notify(
                            NotificationTarget::Character(rec_id),
                            "OnNewMailReceived",
                            "charid",
                            vec![PyRep::Integer(mid), PyRep::Integer(char_id)],
                        );
                    }
                    ctx.notify(
                        NotificationTarget::Character(recipient_id),
                        "OnMailReceived",
                        "charid",
                        vec![PyRep::Integer(mid), PyRep::Integer(char_id)],
                    );
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnMailSent",
                        "charid",
                        vec![PyRep::Integer(mid), PyRep::Integer(recipient_id)],
                    );
                    Some(Ok(PyRep::Integer(mid)))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("mailMgr", "GetHeaders") | ("mailMgr", "GetMailHeaders") | ("mailMgr", "SyncMail") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let (rows, status_rows) = ctx.db.with_snapshot(|snap| {
                let mut hdrs = Vec::new();
                let mut statuses = Vec::new();
                for m in snap.mails.iter().filter(|m| {
                    (m.recipient_id == char_id || m.sender_id == char_id)
                        && !snap.deleted_mails.contains(&(char_id, m.message_id))
                }) {
                    let read = snap.read_mails.contains(&(char_id, m.message_id));
                    let labels = snap
                        .mail_assigned_labels
                        .get(&(char_id, m.message_id))
                        .cloned()
                        .unwrap_or_else(|| vec![if m.sender_id == char_id { 2 } else { 1 }]);
                    let label_mask: i64 = labels.iter().copied().fold(0i64, |acc, l| acc | l);
                    hdrs.push(vec![
                        PyRep::Integer(m.message_id),
                        PyRep::Integer(m.sender_id),
                        PyRep::List(vec![PyRep::Integer(m.recipient_id)]),
                        PyRep::String(m.subject.clone()),
                        PyRep::Integer(133_000_000_000_000_000), // Standard BlueOS filetime
                        PyRep::Bool(read),
                        PyRep::List(labels.into_iter().map(PyRep::Integer).collect()),
                    ]);
                    statuses.push(vec![
                        PyRep::Integer(m.message_id),
                        PyRep::Integer(if read { 1 } else { 0 }),
                        PyRep::Integer(label_mask),
                        PyRep::Bool(read),
                    ]);
                }
                (hdrs, statuses)
            });

            let headers_rowset = PyRep::rowset(
                vec!["messageID", "senderID", "toCharacterIDs", "title", "sentDate", "read", "labels"],
                rows,
            );
            if method == "SyncMail" {
                let old_mail_rowset = PyRep::rowset(
                    vec!["messageID", "senderID", "toCharacterIDs", "title", "sentDate", "read", "labels"],
                    Vec::new(),
                );
                let status_rowset = PyRep::rowset(
                    vec!["messageID", "statusMask", "labelMask", "read"],
                    status_rows,
                );
                Some(Ok(PyRep::keyval([
                    ("oldMail", old_mail_rowset),
                    ("newMail", headers_rowset),
                    ("mailStatus", status_rowset),
                ])))
            } else {
                Some(Ok(headers_rowset))
            }
        }

        ("mailMgr", "GetBody") | ("mailMgr", "GetMailBody") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(mail_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetBody requires messageID".into(),
                }));
            };

            let mail_opt = ctx.db.try_with_mut_snapshot(|snap| {
                let m = snap.mails.iter().find(|m| m.message_id == mail_id).cloned();
                if let Some(ref mail) = m {
                    if mail.recipient_id == char_id {
                        snap.read_mails.insert((char_id, mail_id));
                    }
                }
                Ok::<_, std::convert::Infallible>(m)
            }).unwrap();

            let Some(mail) = mail_opt else {
                return Some(Err(BridgeRpcError {
                    code: "MailNotFound".into(),
                    message: format!("Mail message {mail_id} not found"),
                }));
            };

            if mail.sender_id != char_id && mail.recipient_id != char_id {
                return Some(Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "Cannot read mail belonging to another character".into(),
                }));
            }

            Some(Ok(PyRep::keyval([
                ("messageID", PyRep::Integer(mail.message_id)),
                ("body", PyRep::String(mail.body)),
                ("read", PyRep::Bool(true)),
            ])))
        }

        ("mailMgr", "MarkRead") | ("mailMgr", "MarkMailRead") | ("mailMgr", "MarkAsRead") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut mail_ids = Vec::new();
            match args.first() {
                Some(PyRep::List(list)) | Some(PyRep::Tuple(list)) => {
                    for item in list {
                        if let Some(id) = item.as_i64() {
                            mail_ids.push(id);
                        }
                    }
                }
                Some(rep) => {
                    if let Some(id) = rep.as_i64() {
                        mail_ids.push(id);
                    }
                }
                None => {}
            }
            if mail_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "MarkAsRead requires messageID or messageIDs list".into(),
                }));
            }

            ctx.db.try_with_mut_snapshot(|snap| {
                for &mid in &mail_ids {
                    snap.read_mails.insert((char_id, mid));
                }
                Ok::<(), std::convert::Infallible>(())
            }).unwrap();

            Some(Ok(PyRep::keyval([
                ("messageID", PyRep::Integer(mail_ids[0])),
                ("messageIDs", PyRep::List(mail_ids.into_iter().map(PyRep::Integer).collect())),
                ("read", PyRep::Bool(true)),
            ])))
        }

        ("mailMgr", "MarkAsUnread") | ("mailMgr", "MarkMailUnread") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut mail_ids = Vec::new();
            match args.first() {
                Some(PyRep::List(list)) | Some(PyRep::Tuple(list)) => {
                    for item in list {
                        if let Some(id) = item.as_i64() {
                            mail_ids.push(id);
                        }
                    }
                }
                Some(rep) => {
                    if let Some(id) = rep.as_i64() {
                        mail_ids.push(id);
                    }
                }
                None => {}
            }
            if mail_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "MarkAsUnread requires messageID or messageIDs list".into(),
                }));
            }

            ctx.db.try_with_mut_snapshot(|snap| {
                for &mid in &mail_ids {
                    snap.read_mails.remove(&(char_id, mid));
                }
                Ok::<(), std::convert::Infallible>(())
            }).unwrap();

            Some(Ok(PyRep::keyval([
                ("messageID", PyRep::Integer(mail_ids[0])),
                ("messageIDs", PyRep::List(mail_ids.into_iter().map(PyRep::Integer).collect())),
                ("read", PyRep::Bool(false)),
            ])))
        }

        ("mailMgr", "MarkAllAsRead") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let count = ctx.db.try_with_mut_snapshot(|snap| {
                let mut count = 0;
                let mids: Vec<i64> = snap.mails.iter().filter(|m| m.recipient_id == char_id).map(|m| m.message_id).collect();
                for mid in mids {
                    if snap.read_mails.insert((char_id, mid)) {
                        count += 1;
                    }
                }
                Ok::<_, std::convert::Infallible>(count)
            }).unwrap();

            Some(Ok(PyRep::keyval([
                ("markedReadCount", PyRep::Integer(count)),
            ])))
        }

        ("mailMgr", "GetLabels") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let (custom_labels, mails, read_mails, assignments) = ctx.db.with_snapshot(|snap| {
                (
                    snap.mail_labels.get(&char_id).cloned().unwrap_or_default(),
                    snap.mails.clone(),
                    snap.read_mails.clone(),
                    snap.mail_assigned_labels.clone(),
                )
            });

            let mut all_labels = vec![
                MailLabelRecord { label_id: 1, name: "Inbox".into(), color: "#ffffff".into() },
                MailLabelRecord { label_id: 2, name: "Sent".into(), color: "#ffffff".into() },
                MailLabelRecord { label_id: 4, name: "Corp".into(), color: "#0099ff".into() },
                MailLabelRecord { label_id: 8, name: "Alliance".into(), color: "#00ff33".into() },
            ];
            all_labels.extend(custom_labels);

            let rows: Vec<Vec<PyRep>> = all_labels.into_iter().map(|lbl| {
                let unread_count = mails.iter().filter(|m| {
                    if m.recipient_id != char_id { return false; }
                    if read_mails.contains(&(char_id, m.message_id)) { return false; }
                    let assigned = assignments.get(&(char_id, m.message_id)).cloned().unwrap_or_else(|| vec![1]);
                    assigned.contains(&lbl.label_id)
                }).count();

                vec![
                    PyRep::Integer(lbl.label_id),
                    PyRep::String(lbl.name),
                    PyRep::String(lbl.color),
                    PyRep::Integer(unread_count as i64),
                ]
            }).collect();

            Some(Ok(PyRep::rowset(
                vec!["labelID", "name", "color", "unreadCount"],
                rows,
            )))
        }

        ("mailMgr", "CreateLabel") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let name = args.first().and_then(|a| a.as_str()).unwrap_or("CustomLabel");
            let color = args.get(1).and_then(|a| a.as_str()).unwrap_or("#ffffff");

            let lid = ctx.db.try_with_mut_snapshot(|snap| {
                let list = snap.mail_labels.entry(char_id).or_default();
                let next_id = 16 + (list.len() as i64 * 2);
                list.push(MailLabelRecord {
                    label_id: next_id,
                    name: name.to_string(),
                    color: color.to_string(),
                });
                Ok::<_, std::convert::Infallible>(next_id)
            }).unwrap();

            Some(Ok(PyRep::keyval([
                ("labelID", PyRep::Integer(lid)),
                ("name", PyRep::String(name.to_string())),
                ("color", PyRep::String(color.to_string())),
            ])))
        }

        ("mailMgr", "EditLabel") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(label_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "EditLabel requires labelID".into(),
                }));
            };
            let name = args.get(1).and_then(|a| a.as_str()).unwrap_or("CustomLabel");
            let color = args.get(2).and_then(|a| a.as_str()).unwrap_or("#ffffff");

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(list) = snap.mail_labels.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "LabelNotFound".into(),
                        message: format!("Label {label_id} not found"),
                    });
                };
                let Some(lbl) = list.iter_mut().find(|l| l.label_id == label_id) else {
                    return Err(BridgeRpcError {
                        code: "LabelNotFound".into(),
                        message: format!("Label {label_id} not found"),
                    });
                };
                lbl.name = name.to_string();
                lbl.color = color.to_string();
                Ok(())
            });

            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("labelID", PyRep::Integer(label_id)),
                    ("name", PyRep::String(name.to_string())),
                    ("color", PyRep::String(color.to_string())),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("mailMgr", "DeleteLabel") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(label_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DeleteLabel requires labelID".into(),
                }));
            };
            if label_id <= 8 {
                return Some(Err(BridgeRpcError {
                    code: "CannotDeleteSystemLabel".into(),
                    message: "Default system mail labels cannot be deleted".into(),
                }));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(list) = snap.mail_labels.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "LabelNotFound".into(),
                        message: format!("Label {label_id} not found"),
                    });
                };
                let orig_len = list.len();
                list.retain(|l| l.label_id != label_id);
                if list.len() == orig_len {
                    return Err(BridgeRpcError {
                        code: "LabelNotFound".into(),
                        message: format!("Label {label_id} not found"),
                    });
                }
                Ok(())
            });

            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("labelID", PyRep::Integer(label_id)),
                    ("deleted", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("mailMgr", "AssignLabels") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut mail_ids = Vec::new();
            match args.first() {
                Some(PyRep::List(list)) | Some(PyRep::Tuple(list)) => {
                    for item in list {
                        if let Some(id) = item.as_i64() {
                            mail_ids.push(id);
                        }
                    }
                }
                Some(rep) => {
                    if let Some(id) = rep.as_i64() {
                        mail_ids.push(id);
                    }
                }
                None => {}
            }
            if mail_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "AssignLabels requires messageID or messageIDs list".into(),
                }));
            }
            let mut labels_to_add = Vec::new();
            if let Some(PyRep::List(l)) | Some(PyRep::Tuple(l)) = args.get(1) {
                for item in l {
                    if let Some(lid) = item.as_i64() {
                        labels_to_add.push(lid);
                    }
                }
            } else if let Some(lid) = args.get(1).and_then(|a| a.as_i64()) {
                labels_to_add.push(lid);
            }

            ctx.db.try_with_mut_snapshot(|snap| {
                for &mail_id in &mail_ids {
                    let current = snap.mail_assigned_labels.entry((char_id, mail_id)).or_default();
                    for &lid in &labels_to_add {
                        if !current.contains(&lid) {
                            current.push(lid);
                        }
                    }
                }
                Ok::<(), std::convert::Infallible>(())
            }).unwrap();

            Some(Ok(PyRep::keyval([
                ("messageID", PyRep::Integer(mail_ids[0])),
                ("assigned", PyRep::Bool(true)),
            ])))
        }

        ("mailMgr", "RemoveLabels") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut mail_ids = Vec::new();
            match args.first() {
                Some(PyRep::List(list)) | Some(PyRep::Tuple(list)) => {
                    for item in list {
                        if let Some(id) = item.as_i64() {
                            mail_ids.push(id);
                        }
                    }
                }
                Some(rep) => {
                    if let Some(id) = rep.as_i64() {
                        mail_ids.push(id);
                    }
                }
                None => {}
            }
            if mail_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "RemoveLabels requires messageID or messageIDs list".into(),
                }));
            }
            let mut labels_to_remove = Vec::new();
            if let Some(PyRep::List(l)) | Some(PyRep::Tuple(l)) = args.get(1) {
                for item in l {
                    if let Some(lid) = item.as_i64() {
                        labels_to_remove.push(lid);
                    }
                }
            } else if let Some(lid) = args.get(1).and_then(|a| a.as_i64()) {
                labels_to_remove.push(lid);
            }

            ctx.db.try_with_mut_snapshot(|snap| {
                for &mail_id in &mail_ids {
                    if let Some(current) = snap.mail_assigned_labels.get_mut(&(char_id, mail_id)) {
                        current.retain(|lid| !labels_to_remove.contains(lid));
                    }
                }
                Ok::<(), std::convert::Infallible>(())
            }).unwrap();

            Some(Ok(PyRep::keyval([
                ("messageID", PyRep::Integer(mail_ids[0])),
                ("removed", PyRep::Bool(true)),
            ])))
        }

        ("mailMgr", "DeleteMail") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(mail_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DeleteMail requires messageID".into(),
                }));
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                snap.deleted_mails.insert((char_id, mail_id));
                Ok::<(), std::convert::Infallible>(())
            }).unwrap();

            Some(Ok(PyRep::keyval([
                ("messageID", PyRep::Integer(mail_id)),
                ("deleted", PyRep::Bool(true)),
            ])))
        }

        // =========================================================================
        // SO-5: Bookmarks (`bookmark`, `bookmarkMgr`, `accessGroupBookmarkMgr`)
        // =========================================================================
        ("bookmark", "BookmarkLocation") | ("bookmarkMgr", "BookmarkLocation") | ("accessGroupBookmarkMgr", "BookmarkStaticLocation") | ("bookmark", "BookmarkEntity") | ("bookmarkMgr", "BookmarkEntity") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let label = args.first().and_then(|a| a.as_str()).unwrap_or("Safe Spot").trim();
            if label.is_empty() || label.chars().count() > 100 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidBookmarkLabel".into(),
                    message: "Bookmark label must be 1..=100 non-whitespace characters".into(),
                }));
            }
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };

            // Derive bookmark coordinates
            let (explicit_entity_id, memo, folder_id) = if args.get(1).and_then(|a| a.as_str()).is_some() {
                (
                    args.get(3).and_then(|a| a.as_i64()),
                    args.get(1).and_then(|a| a.as_str()).unwrap_or("").to_string(),
                    args.get(2).and_then(|a| a.as_i64()),
                )
            } else {
                (
                    args.get(1).and_then(|a| a.as_i64()),
                    args.get(2).and_then(|a| a.as_str()).unwrap_or("").to_string(),
                    args.get(3).and_then(|a| a.as_i64()),
                )
            };

            let (x, y, z) = if let Some(ent_id) = explicit_entity_id {
                let sys_handle = ctx.systems.get_or_spawn(sys_id);
                if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(ent_id).await {
                    (ball.position.x, ball.position.y, ball.position.z)
                } else if let Some(st) = ctx.sde.solar_systems.get(&sys_id).and_then(|s| s.stations.iter().find(|st| st.station_id == ent_id)) {
                    (st.position.x, st.position.y, st.position.z)
                } else {
                    (0.0, 0.0, 0.0)
                }
            } else if let Some(st_id) = session.station_id {
                ctx.sde.solar_systems.get(&sys_id)
                    .and_then(|s| s.stations.iter().find(|st| st.station_id == st_id))
                    .map(|st| (st.position.x, st.position.y, st.position.z))
                    .unwrap_or((0.0, 0.0, 0.0))
            } else if let Some(struct_id) = session.structure_id {
                ctx.db.with_snapshot(|snap| {
                    snap.structures.get(&struct_id).map(|s| (s.position[0], s.position[1], s.position[2]))
                }).unwrap_or((0.0, 0.0, 0.0))
            } else if let Some(ship_id) = session.ship_id {
                let handle = ctx.systems.get_or_spawn(sys_id);
                if let Ok(Some((_, ball))) = handle.get_ball_snapshot(ship_id).await {
                    (ball.position.x, ball.position.y, ball.position.z)
                } else {
                    return Some(Err(BridgeRpcError {
                        code: "ShipNotInSpace".into(),
                        message: "Cannot bookmark in-space position when ship ball is absent".into(),
                    }));
                }
            } else {
                (0.0, 0.0, 0.0)
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let count = snap.bookmarks.values().filter(|b| b.owner_id == char_id).count();
                if count >= 500 {
                    return Err(BridgeRpcError {
                        code: "MaxBookmarksReached".into(),
                        message: "Character has reached the maximum of 500 personal bookmarks".into(),
                    });
                }
                let is_shared_folder = folder_id.and_then(|fid| snap.bookmark_folders.get(&fid)).map(|f| f.is_shared).unwrap_or(false);
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.bookmarks.insert(
                    id,
                    BookmarkRecord {
                        bookmark_id: id,
                        owner_id: char_id,
                        label: label.to_string(),
                        solar_system_id: sys_id,
                        x,
                        y,
                        z,
                        item_id: explicit_entity_id,
                        memo,
                        folder_id,
                        is_shared: is_shared_folder,
                    },
                );
                Ok(id)
            });

            match res {
                Ok(bm_id) => Some(Ok(PyRep::keyval([
                    ("bookmarkID", PyRep::Integer(bm_id)),
                    ("label", PyRep::String(label.to_string())),
                    ("x", PyRep::Real(x)),
                    ("y", PyRep::Real(y)),
                    ("z", PyRep::Real(z)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "GetBookmarks") | ("bookmarkMgr", "GetBookmarks") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id_opt = session.corporation_id;
            let rows = ctx.db.with_snapshot(|snap| {
                snap.bookmarks
                    .values()
                    .filter(|b| {
                        b.owner_id == char_id || (b.is_shared && corp_id_opt.is_some())
                    })
                    .map(|b| {
                        vec![
                            PyRep::Integer(b.bookmark_id),
                            PyRep::String(b.label.clone()),
                            PyRep::Integer(b.solar_system_id as i64),
                            PyRep::Real(b.x),
                            PyRep::Real(b.y),
                            PyRep::Real(b.z),
                            b.item_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                            PyRep::String(b.memo.clone()),
                            b.folder_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                            PyRep::Bool(b.is_shared),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["bookmarkID", "label", "solarSystemID", "x", "y", "z", "itemID", "memo", "folderID", "isShared"],
                rows,
            )))
        }

        ("bookmark", "UpdateBookmark") | ("bookmarkMgr", "UpdateBookmark") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(bm_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "UpdateBookmark requires bookmarkID".into(),
                }));
            };
            let label_opt = args.get(1).and_then(|a| a.as_str());
            let memo_opt = args.get(2).and_then(|a| a.as_str());
            let folder_id_opt = args.get(3).and_then(|a| a.as_i64());

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(bm) = snap.bookmarks.get_mut(&bm_id) else {
                    return Err(BridgeRpcError {
                        code: "BookmarkNotFound".into(),
                        message: format!("Bookmark {bm_id} not found"),
                    });
                };
                if bm.owner_id != char_id && !bm.is_shared {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Cannot edit another character's bookmark".into(),
                    });
                }
                if let Some(l) = label_opt {
                    bm.label = l.to_string();
                }
                if let Some(m) = memo_opt {
                    bm.memo = m.to_string();
                }
                if let Some(fid) = folder_id_opt {
                    bm.folder_id = Some(fid);
                }
                Ok(())
            });

            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("bookmarkID", PyRep::Integer(bm_id)),
                    ("updated", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "DeleteBookmark") | ("bookmark", "DeleteBookmarks") | ("bookmarkMgr", "DeleteBookmark") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut bm_ids = Vec::new();
            match args.first() {
                Some(PyRep::List(list)) | Some(PyRep::Tuple(list)) => {
                    for item in list {
                        if let Some(id) = item.as_i64() {
                            bm_ids.push(id);
                        }
                    }
                }
                Some(rep) => {
                    if let Some(id) = rep.as_i64() {
                        bm_ids.push(id);
                    }
                }
                None => {}
            }
            if bm_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DeleteBookmark requires bookmarkID".into(),
                }));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                for &bm_id in &bm_ids {
                    let Some(bm) = snap.bookmarks.get(&bm_id) else {
                        return Err(BridgeRpcError {
                            code: "BookmarkNotFound".into(),
                            message: format!("Bookmark {bm_id} not found"),
                        });
                    };
                    if bm.owner_id != char_id && !bm.is_shared {
                        return Err(BridgeRpcError {
                            code: "AccessDenied".into(),
                            message: "Cannot delete another character's bookmark".into(),
                        });
                    }
                    snap.bookmarks.remove(&bm_id);
                }
                Ok(())
            });

            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("bookmarkID", PyRep::Integer(bm_ids[0])),
                    ("deleted", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "CreateFolder") | ("bookmark", "AddFolder") | ("bookmarkMgr", "CreateFolder") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let name = args.first().and_then(|a| a.as_str()).unwrap_or("New Folder");

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let personal_root_count = snap
                    .bookmark_folders
                    .values()
                    .filter(|f| f.owner_id == char_id && !f.is_shared && !f.folder_name.contains('/'))
                    .count();
                if personal_root_count >= 5 {
                    return Err(BridgeRpcError {
                        code: "MaxPersonalFoldersReached".into(),
                        message: "Character has reached the maximum of 5 personal bookmark folders".into(),
                    });
                }
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.bookmark_folders.insert(
                    id,
                    BookmarkFolderRecord {
                        folder_id: id,
                        owner_id: char_id,
                        folder_name: name.to_string(),
                        is_shared: false,
                        corporation_id: None,
                    },
                );
                Ok(id)
            });

            match res {
                Ok(folder_id) => Some(Ok(PyRep::keyval([
                    ("folderID", PyRep::Integer(folder_id)),
                    ("folderName", PyRep::String(name.to_string())),
                    ("isShared", PyRep::Bool(false)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "GetFolders") | ("bookmarkMgr", "GetFolders") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id_opt = session.corporation_id;

            let rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                snap.bookmark_folders
                    .values()
                    .filter(|f| {
                        f.owner_id == char_id || (f.is_shared && f.corporation_id == corp_id_opt)
                    })
                    .map(|f| {
                        vec![
                            PyRep::Integer(f.folder_id),
                            PyRep::String(f.folder_name.clone()),
                            PyRep::Integer(f.owner_id),
                            PyRep::Bool(f.is_shared),
                        ]
                    })
                    .collect()
            });

            Some(Ok(PyRep::rowset(
                vec!["folderID", "folderName", "ownerID", "isShared"],
                rows,
            )))
        }

        ("bookmark", "DeleteFolder") | ("bookmarkMgr", "DeleteFolder") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(folder_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DeleteFolder requires folderID".into(),
                }));
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(f) = snap.bookmark_folders.get(&folder_id) else {
                    return Err(BridgeRpcError {
                        code: "FolderNotFound".into(),
                        message: format!("Folder {folder_id} not found"),
                    });
                };
                if f.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Cannot delete another character's folder".into(),
                    });
                }
                snap.bookmark_folders.remove(&folder_id);
                for bm in snap.bookmarks.values_mut() {
                    if bm.folder_id == Some(folder_id) {
                        bm.folder_id = None;
                    }
                }
                Ok(())
            });

            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("folderID", PyRep::Integer(folder_id)),
                    ("deleted", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("accessGroupBookmarkMgr", "CreateSharedFolder") | ("bookmark", "CreateSharedFolder") | ("bookmarkMgr", "CreateSharedFolder") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let db_corp_id = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .filter(|cid| snap.corporations.contains_key(cid))
            });
            if let Some(cid) = db_corp_id {
                session.corporation_id = Some(cid);
            }
            let corp_id = match session.corporation_id {
                Some(c) => c,
                None => return Some(Err(BridgeRpcError {
                    code: "NoCorporation".into(),
                    message: "Creating shared folder requires corporation membership".into(),
                })),
            };
            let name = args.first().and_then(|a| a.as_str()).unwrap_or("Shared Corp Bookmarks");

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let has_rights = if let Some(corp) = snap.corporations.get(&corp_id) {
                    let role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                    char_id == corp.ceo_id
                        || corp_roles::has_role(role, corp_roles::ROLE_DIRECTOR)
                        || corp_roles::has_role(role, corp_roles::ROLE_PERSONNEL_MANAGER)
                } else {
                    corp_roles::has_role(session.corp_role, corp_roles::ROLE_DIRECTOR)
                        || corp_roles::has_role(session.corp_role, corp_roles::ROLE_PERSONNEL_MANAGER)
                };
                if !has_rights {
                    return Err(BridgeRpcError {
                        code: "BookmarkAccessDenied".into(),
                        message: "Creating a shared bookmark folder requires corporation Director/CEO access-group rights".into(),
                    });
                }
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.bookmark_folders.insert(
                    id,
                    BookmarkFolderRecord {
                        folder_id: id,
                        owner_id: char_id,
                        folder_name: name.to_string(),
                        is_shared: true,
                        corporation_id: Some(corp_id),
                    },
                );
                Ok(id)
            });

            let folder_id = match res {
                Ok(id) => id,
                Err(e) => return Some(Err(e)),
            };

            ctx.notify(
                NotificationTarget::Corporation(corp_id),
                "OnSharedBookmarksFolderUpdated",
                "corpid",
                vec![PyRep::Integer(folder_id), PyRep::Integer(corp_id)],
            );

            Some(Ok(PyRep::keyval([
                ("folderID", PyRep::Integer(folder_id)),
                ("folderName", PyRep::String(name.to_string())),
                ("isShared", PyRep::Bool(true)),
                ("corporationID", PyRep::Integer(corp_id)),
            ])))
        }

        ("accessGroupBookmarkMgr", "GetSharedFolders") | ("bookmark", "GetSharedFolders") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let db_corp_id = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .filter(|cid| snap.corporations.contains_key(cid))
            });
            if let Some(cid) = db_corp_id {
                session.corporation_id = Some(cid);
            }
            let corp_id_opt = session.corporation_id;

            let rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                snap.bookmark_folders
                    .values()
                    .filter(|f| f.is_shared && f.corporation_id == corp_id_opt)
                    .map(|f| {
                        vec![
                            PyRep::Integer(f.folder_id),
                            PyRep::String(f.folder_name.clone()),
                            f.corporation_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                        ]
                    })
                    .collect()
            });

            Some(Ok(PyRep::rowset(
                vec!["folderID", "folderName", "corporationID"],
                rows,
            )))
        }

        ("scanMgr", "BookmarkScanResult") | ("scanMgr", "BookmarkSignature") | ("bookmark", "BookmarkScanResult") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sig_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "BookmarkScanResult requires signatureID".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let label = args.get(1).and_then(|a| a.as_str()).unwrap_or(sig_id);
            let folder_id = args.get(2).and_then(|a| a.as_i64());

            let sig_res = ctx.db.with_snapshot(|snap| {
                snap.cosmic_signatures.get(&sys_id).and_then(|sigs| {
                    sigs.iter()
                        .find(|s| s.signature_id == sig_id)
                        .map(|s| (s.position, s.signal_strength))
                })
            });

            let Some((pos, signal_strength)) = sig_res else {
                return Some(Err(BridgeRpcError {
                    code: "SignatureNotFound".into(),
                    message: format!("Cosmic signature {sig_id} not found in system {sys_id}"),
                }));
            };
            if signal_strength < 1.0 {
                return Some(Err(BridgeRpcError {
                    code: "SignatureNotResolved".into(),
                    message: format!("Cosmic signature {sig_id} must be resolved to 100% before bookmarking"),
                }));
            }

            let bm_id = ctx.db.try_with_mut_snapshot(|snap| {
                let is_shared_folder = folder_id
                    .and_then(|fid| snap.bookmark_folders.get(&fid))
                    .map(|f| f.is_shared)
                    .unwrap_or(false);
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.bookmarks.insert(
                    id,
                    BookmarkRecord {
                        bookmark_id: id,
                        owner_id: char_id,
                        label: label.to_string(),
                        solar_system_id: sys_id,
                        x: pos[0],
                        y: pos[1],
                        z: pos[2],
                        item_id: None,
                        memo: format!("Scanned cosmic signature {sig_id}"),
                        folder_id,
                        is_shared: is_shared_folder,
                    },
                );
                Ok::<_, std::convert::Infallible>(id)
            }).unwrap();

            Some(Ok(PyRep::keyval([
                ("bookmarkID", PyRep::Integer(bm_id)),
                ("signatureID", PyRep::String(sig_id.to_string())),
                ("label", PyRep::String(label.to_string())),
                ("x", PyRep::Real(pos[0])),
                ("y", PyRep::Real(pos[1])),
                ("z", PyRep::Real(pos[2])),
            ])))
        }

        // =========================================================================
        // SO-6 & SO-7: Probing & Wormholes (`scanMgr`, `probeMgr`, `wormholeMgr`)
        // =========================================================================
        ("scanMgr", "GetSystemSignatures") | ("scanMgr", "GetSignatures") | ("probeMgr", "GetSystemSignatures") => {
            let (_, _) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = args.first().and_then(|a| a.as_i64()).map(|v| v as u32).or(session.solar_system_id).unwrap_or(30_000_142);
            let now_unix = ctx.now_unix();

            let signatures = ctx.db.try_with_mut_snapshot(|snap| {
                let list = snap.cosmic_signatures.entry(sys_id).or_default();
                if !list.iter().any(|s| s.sig_group == "Wormhole") {
                    list.extend([
                        CosmicSignatureRecord {
                            signature_id: format!("JMM-{}", 100 + (sys_id % 900)),
                            solar_system_id: sys_id,
                            sig_group: "Wormhole".into(),
                            type_name: "Unstable Wormhole (K162)".into(),
                            position: [12.0 * AU_IN_METERS, 2.5 * AU_IN_METERS, -4.0 * AU_IN_METERS],
                            signal_strength: 0.0,
                            wormhole_type_id: Some(26272),
                            destination_system_id: Some(31_000_005),
                            created_unix: now_unix,
                            lifetime_seconds: 86_400,
                            max_mass_kg: 2_000_000_000.0,
                            remaining_mass_kg: 2_000_000_000.0,
                            max_jump_mass_kg: 375_000_000.0,
                            collapsed: false,
                        },
                        CosmicSignatureRecord {
                            signature_id: format!("REL-{}", 200 + (sys_id % 700)),
                            solar_system_id: sys_id,
                            sig_group: "Relic Site".into(),
                            type_name: "Ruined Sansha Relic Site".into(),
                            position: [-8.0 * AU_IN_METERS, 1.0 * AU_IN_METERS, 15.0 * AU_IN_METERS],
                            signal_strength: 0.0,
                            wormhole_type_id: None,
                            destination_system_id: None,
                            created_unix: now_unix,
                            lifetime_seconds: 86_400,
                            max_mass_kg: 0.0,
                            remaining_mass_kg: 0.0,
                            max_jump_mass_kg: 0.0,
                            collapsed: false,
                        },
                        CosmicSignatureRecord {
                            signature_id: format!("DAT-{}", 300 + (sys_id % 600)),
                            solar_system_id: sys_id,
                            sig_group: "Data Site".into(),
                            type_name: "Local Information Repository".into(),
                            position: [5.0 * AU_IN_METERS, -11.0 * AU_IN_METERS, 3.0 * AU_IN_METERS],
                            signal_strength: 0.0,
                            wormhole_type_id: None,
                            destination_system_id: None,
                            created_unix: now_unix,
                            lifetime_seconds: 86_400,
                            max_mass_kg: 0.0,
                            remaining_mass_kg: 0.0,
                            max_jump_mass_kg: 0.0,
                            collapsed: false,
                        },
                        CosmicSignatureRecord {
                            signature_id: format!("CBT-{}", 400 + (sys_id % 500)),
                            solar_system_id: sys_id,
                            sig_group: "Combat Site".into(),
                            type_name: "Pirate Fleet Base".into(),
                            position: [-14.0 * AU_IN_METERS, 0.0 * AU_IN_METERS, -8.0 * AU_IN_METERS],
                            signal_strength: 0.0,
                            wormhole_type_id: None,
                            destination_system_id: None,
                            created_unix: now_unix,
                            lifetime_seconds: 86_400,
                            max_mass_kg: 0.0,
                            remaining_mass_kg: 0.0,
                            max_jump_mass_kg: 0.0,
                            collapsed: false,
                        },
                        CosmicSignatureRecord {
                            signature_id: format!("GAS-{}", 500 + (sys_id % 400)),
                            solar_system_id: sys_id,
                            sig_group: "Gas Site".into(),
                            type_name: "Amber Cytoserocin Nebula".into(),
                            position: [9.0 * AU_IN_METERS, -4.0 * AU_IN_METERS, 7.0 * AU_IN_METERS],
                            signal_strength: 0.0,
                            wormhole_type_id: None,
                            destination_system_id: None,
                            created_unix: now_unix,
                            lifetime_seconds: 86_400,
                            max_mass_kg: 0.0,
                            remaining_mass_kg: 0.0,
                            max_jump_mass_kg: 0.0,
                            collapsed: false,
                        },
                        CosmicSignatureRecord {
                            signature_id: format!("ORE-{}", 600 + (sys_id % 300)),
                            solar_system_id: sys_id,
                            sig_group: "Ore Site".into(),
                            type_name: "Rare Ore Site (Jaspet & Hemorphite)".into(),
                            position: [-11.0 * AU_IN_METERS, 6.0 * AU_IN_METERS, -13.0 * AU_IN_METERS],
                            signal_strength: 0.0,
                            wormhole_type_id: None,
                            destination_system_id: None,
                            created_unix: now_unix,
                            lifetime_seconds: 86_400,
                            max_mass_kg: 0.0,
                            remaining_mass_kg: 0.0,
                            max_jump_mass_kg: 0.0,
                            collapsed: false,
                        },
                    ]);
                }
                Ok::<_, std::convert::Infallible>(list.clone())
            }).unwrap();

            let rows: Vec<Vec<PyRep>> = signatures.into_iter().map(|sig| {
                let group = if sig.signal_strength >= 0.25 { sig.sig_group } else { "Unknown".into() };
                let type_name = if sig.signal_strength >= 1.0 { sig.type_name } else { "Unidentified Signature".into() };
                vec![
                    PyRep::String(sig.signature_id),
                    PyRep::String(group),
                    PyRep::String(type_name),
                    PyRep::Real(sig.signal_strength),
                ]
            }).collect();

            Some(Ok(PyRep::rowset(
                vec!["signatureID", "group", "type", "signalStrength"],
                rows,
            )))
        }

        ("scanMgr", "ScanWithProbes") | ("probeMgr", "ScanWithProbes") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };

            let probe_positions_rep = args.first();
            let scan_radius_meters = args.get(1).and_then(|a| a.as_f64()).unwrap_or(4.0 * AU_IN_METERS).max(0.25 * AU_IN_METERS);
            let base_probe_strength = args.get(2).and_then(|a| a.as_f64()).unwrap_or(40.0);

            // Parse probe positions (defaults to pinpoint around 12 AU if omitted)
            let mut probes = Vec::new();
            if let Some(PyRep::List(list)) = probe_positions_rep {
                for item in list {
                    if let PyRep::List(coords) = item {
                        if coords.len() >= 3 {
                            let x = coords[0].as_f64().unwrap_or(0.0);
                            let y = coords[1].as_f64().unwrap_or(0.0);
                            let z = coords[2].as_f64().unwrap_or(0.0);
                            probes.push(DVec3::new(x, y, z));
                        }
                    }
                }
            }
            if probes.is_empty() {
                // Generate default 8-probe tetrahedral pinpoint formation around center
                let center = DVec3::new(12.0 * AU_IN_METERS, 2.5 * AU_IN_METERS, -4.0 * AU_IN_METERS);
                let d = scan_radius_meters * 0.5;
                probes = vec![
                    center,
                    center + DVec3::new(d, 0.0, 0.0),
                    center + DVec3::new(-d, 0.0, 0.0),
                    center + DVec3::new(0.0, d, 0.0),
                    center + DVec3::new(0.0, -d, 0.0),
                    center + DVec3::new(0.0, 0.0, d),
                    center + DVec3::new(0.0, 0.0, -d),
                    center + DVec3::new(d * 0.5, d * 0.5, d * 0.5),
                ];
            }

            let astrometrics_level = ctx.db.with_snapshot(|snap| {
                snap.skills.get(&char_id).and_then(|s| s.get(&3412)).map(|sk| sk.level).unwrap_or(3)
            });
            let skill_mult = 1.0 + (astrometrics_level as f64 * 0.05);
            let radius_au = scan_radius_meters / AU_IN_METERS;

            let updated_results = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id) else {
                    return Err(BridgeRpcError {
                        code: "NoSignaturesInSystem".into(),
                        message: format!("No cosmic signatures initialized in solar system {sys_id}"),
                    });
                };

                let mut out = Vec::new();
                for sig in sigs.iter_mut() {
                    let sig_pos = DVec3::new(sig.position[0], sig.position[1], sig.position[2]);
                    let mut probes_in_range = 0;
                    let mut total_probe_contrib = 0.0;

                    for probe in &probes {
                        let dist = probe.distance(sig_pos);
                        if dist <= scan_radius_meters {
                            probes_in_range += 1;
                            let dist_factor = (1.0 - (dist / scan_radius_meters)).max(0.0);
                            let probe_signal = dist_factor * (1.0 / radius_au) * (base_probe_strength / 40.0) * skill_mult;
                            total_probe_contrib += probe_signal;
                        }
                    }

                    let coverage_mult = if probes_in_range >= 4 {
                        0.50 // 3D tetrahedral fix
                    } else if probes_in_range >= 2 {
                        0.20
                    } else if probes_in_range == 1 {
                        0.05
                    } else {
                        0.0
                    };

                    let signal_gain = total_probe_contrib * coverage_mult;
                    sig.signal_strength = (sig.signal_strength + signal_gain).clamp(0.0, 1.0);
                    let resolved = sig.signal_strength >= 1.0;

                    out.push((
                        sig.signature_id.clone(),
                        sig.sig_group.clone(),
                        sig.type_name.clone(),
                        sig.signal_strength,
                        resolved,
                        sig.position,
                    ));
                }
                Ok(out)
            });

            match updated_results {
                Ok(results) => {
                    let rows: Vec<Vec<PyRep>> = results.into_iter().map(|(id, grp, tname, strn, resolved, pos)| {
                        vec![
                            PyRep::String(id),
                            PyRep::String(if strn >= 0.25 { grp } else { "Unknown".into() }),
                            PyRep::String(if resolved { tname } else { "Unidentified Signature".into() }),
                            PyRep::Real(strn),
                            PyRep::Bool(resolved),
                            if resolved { PyRep::Real(pos[0]) } else { PyRep::None },
                            if resolved { PyRep::Real(pos[1]) } else { PyRep::None },
                            if resolved { PyRep::Real(pos[2]) } else { PyRep::None },
                        ]
                    }).collect();

                    Some(Ok(PyRep::rowset(
                        vec!["signatureID", "group", "type", "signalStrength", "resolved", "x", "y", "z"],
                        rows,
                    )))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("wormholeMgr", "GetWormholeInfo") | ("wormholeMgr", "GetWormholeStatus") => {
            let (_, _) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sig_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetWormholeInfo requires signatureID string".into(),
                }));
            };
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            let now_unix = ctx.now_unix();

            let wh_opt = ctx.db.with_snapshot(|snap| {
                snap.cosmic_signatures.get(&sys_id).and_then(|list| {
                    list.iter().find(|s| s.signature_id == sig_id && s.sig_group == "Wormhole").cloned()
                })
            });

            let Some(wh) = wh_opt else {
                return Some(Err(BridgeRpcError {
                    code: "WormholeNotFound".into(),
                    message: format!("Wormhole signature {sig_id} not found in system {sys_id}"),
                }));
            };

            // Lifetime tracking
            let elapsed = now_unix.saturating_sub(wh.created_unix);
            let lifetime_pct = if elapsed >= wh.lifetime_seconds {
                0.0
            } else {
                (wh.lifetime_seconds - elapsed) as f64 / wh.lifetime_seconds as f64
            };

            let lifetime_stage = if lifetime_pct <= 0.0 {
                "Collapsed_Expired"
            } else if lifetime_pct < 0.25 {
                "ReachingEndOfLifetime" // EOL
            } else {
                "Healthy"
            };

            // Mass tracking
            let mass_pct = if wh.max_mass_kg > 0.0 {
                wh.remaining_mass_kg / wh.max_mass_kg
            } else {
                0.0
            };

            let mass_stage = if wh.remaining_mass_kg <= 0.0 {
                "Collapsed_Exhausted"
            } else if mass_pct < 0.10 {
                "Critical" // Stage 3: on the verge of collapse
            } else if mass_pct < 0.50 {
                "Destabilized" // Stage 2: reduced stability
            } else {
                "Stable" // Stage 1: stability not significantly disrupted
            };

            let is_collapsed = wh.collapsed || lifetime_pct <= 0.0 || wh.remaining_mass_kg <= 0.0;

            Some(Ok(PyRep::keyval([
                ("signatureID", PyRep::String(wh.signature_id)),
                ("lifetimeStage", PyRep::String(lifetime_stage.into())),
                ("massStage", PyRep::String(mass_stage.into())),
                ("remainingMassKg", PyRep::Real(wh.remaining_mass_kg)),
                ("maxJumpMassKg", PyRep::Real(wh.max_jump_mass_kg)),
                ("destinationSystemID", wh.destination_system_id.map(|d| PyRep::Integer(d as i64)).unwrap_or(PyRep::None)),
                ("collapsed", PyRep::Bool(is_collapsed)),
            ])))
        }

        ("wormholeMgr", "JumpWormhole") | ("wormholeMgr", "WormholeJump") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sig_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "JumpWormhole requires signatureID".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };

            // Query ship ball
            let handle = ctx.systems.get_or_spawn(sys_id);
            let Some((_, current_ball)) = handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: "Active ship ball is not in space".into(),
                }));
            };

            let ship_mass = current_ball.mass;
            let now_unix = ctx.now_unix();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id) else {
                    return Err(BridgeRpcError {
                        code: "WormholeNotFound".into(),
                        message: "No signatures in system".into(),
                    });
                };
                let Some(wh) = sigs.iter_mut().find(|s| s.signature_id == sig_id && s.sig_group == "Wormhole") else {
                    return Err(BridgeRpcError {
                        code: "WormholeNotFound".into(),
                        message: format!("Wormhole {sig_id} not found in system {sys_id}"),
                    });
                };

                let dest_sys = wh.destination_system_id.unwrap_or(31_000_005);

                // Check polarization
                let pol_expiry = snap.wh_polarizations.get(&(char_id, dest_sys as i64)).copied().unwrap_or(0);
                if pol_expiry > now_unix {
                    return Err(BridgeRpcError {
                        code: "WormholePolarized".into(),
                        message: format!("Character is polarized for system {} for another {} seconds", dest_sys, pol_expiry.saturating_sub(now_unix)),
                    });
                }

                // Check distance
                let wh_pos = DVec3::new(wh.position[0], wh.position[1], wh.position[2]);
                let dist = current_ball.position.distance(wh_pos);
                if dist > 2_500.0 {
                    return Err(BridgeRpcError {
                        code: "OutsideWormholeJumpRange".into(),
                        message: format!("Ship is {:.1} m from wormhole (must be <= 2500 m)", dist),
                    });
                }

                // Check lifetime
                let elapsed = now_unix.saturating_sub(wh.created_unix);
                if elapsed >= wh.lifetime_seconds || wh.collapsed {
                    wh.collapsed = true;
                    return Err(BridgeRpcError {
                        code: "WormholeCollapsed".into(),
                        message: "Wormhole has collapsed due to natural lifetime expiry".into(),
                    });
                }

                // Check max jump mass
                if wh.max_jump_mass_kg > 0.0 && ship_mass > wh.max_jump_mass_kg {
                    return Err(BridgeRpcError {
                        code: "ShipTooHeavyForWormhole".into(),
                        message: format!("Ship mass {:.1} kg exceeds wormhole jump limit {:.1} kg", ship_mass, wh.max_jump_mass_kg),
                    });
                }

                // Check remaining mass capacity
                if wh.remaining_mass_kg < ship_mass {
                    wh.collapsed = true;
                    return Err(BridgeRpcError {
                        code: "WormholeMassCollapsed".into(),
                        message: "Wormhole lacks remaining mass capacity and has collapsed".into(),
                    });
                }

                // Deduct mass
                wh.remaining_mass_kg -= ship_mass;
                if wh.remaining_mass_kg <= 0.0 {
                    wh.collapsed = true;
                }

                // Insert 300s polarization
                snap.wh_polarizations.insert((char_id, dest_sys as i64), now_unix + 300);

                Ok((dest_sys, wh.remaining_mass_kg, wh.collapsed))
            });

            match res {
                Ok((dest_sys_id, remaining_mass, is_collapsed)) => {
                    let _ = handle.remove_ball(ship_id).await;
                    let _ = ctx.db.set_character_solar_system(char_id, dest_sys_id, &ctx.sde);
                    ctx.sync_character_local_chat(char_id, Some(sys_id), dest_sys_id);
                    session.solar_system_id = Some(dest_sys_id);

                    let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
                    let mut new_ball = current_ball;
                    new_ball.position = DVec3::new(10_000.0, 0.0, 0.0);
                    new_ball.velocity = DVec3::ZERO;
                    new_ball.speed_fraction = 0.0;
                    new_ball.mode = MovementMode::Stop;
                    let combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
                    let _ = new_handle.spawn_ball_with_combat(new_ball, combat_state).await;

                    Some(Ok(PyRep::keyval([
                        ("success", PyRep::Bool(true)),
                        ("destinationSystemID", PyRep::Integer(dest_sys_id as i64)),
                        ("remainingMassKg", PyRep::Real(remaining_mass)),
                        ("collapsed", PyRep::Bool(is_collapsed)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("wormholeMgr", "GetWormholePolarization") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_sys = args.first().and_then(|a| a.as_i64()).unwrap_or(31_000_005);
            let now_unix = ctx.now_unix();
            let remaining = ctx.db.with_snapshot(|snap| {
                snap.wh_polarizations.get(&(char_id, target_sys)).copied().unwrap_or(0).saturating_sub(now_unix)
            });
            let is_polarized = remaining > 0;
            Some(Ok(PyRep::keyval([
                ("destinationSystemID", PyRep::Integer(target_sys)),
                ("isPolarized", PyRep::Bool(is_polarized)),
                ("remainingSeconds", PyRep::Integer(remaining as i64)),
            ])))
        }

        ("wormholeMgr", "GetSystemStatics") => {
            let sys_id = args.first().and_then(|a| a.as_i64()).map(|v| v as u32).or(session.solar_system_id).unwrap_or(30_000_142);
            let statics = if sys_id == 31_000_005 {
                vec![
                    ("HighSec", "HS-Static", 10000000),
                    ("LowSec", "LS-Static", 10000000),
                    ("NullSec", "NS-Static", 10000000),
                ]
            } else if sys_id >= 31_000_000 && sys_id <= 31_002_605 {
                let wh_class = ((sys_id - 31_000_000) % 6) + 1;
                match wh_class {
                    1 => vec![("C1-Static", "H296", 1)],
                    2 => vec![("C2-Static", "C247", 2)],
                    3 => vec![("C3-Static", "D845", 3)],
                    4 => vec![("C4-Static", "E175", 4)],
                    5 => vec![("C5-Static", "H900", 5)],
                    _ => vec![("C6-Static", "V898", 6)],
                }
            } else {
                vec![("Wandering", "Unknown", 0)]
            };
            let rows = statics.into_iter().map(|(name, code, target)| {
                vec![PyRep::String(name.into()), PyRep::String(code.into()), PyRep::Integer(target)]
            }).collect();
            Some(Ok(PyRep::rowset(vec!["name", "code", "targetClassOrSec"], rows)))
        }

        ("wormholeMgr", "GetSystemWormholeClass") | ("wormholeMgr", "GetWormholeSystemRules") => {
            let sys_id = args.first().and_then(|a| a.as_i64()).map(|v| v as u32).or(session.solar_system_id).unwrap_or(30_000_142);
            let is_wh = sys_id >= 31_000_000 && sys_id <= 31_002_605;
            let is_thera = sys_id == 31_000_005;
            let is_shattered = is_thera || (is_wh && (sys_id % 10 == 0));
            let wh_class = if is_thera {
                12
            } else if is_wh {
                ((sys_id - 31_000_000) % 6) + 1
            } else {
                0
            };
            let delayed_local = is_wh;
            let allows_capitals = !is_wh || (wh_class >= 5 && !is_shattered);
            Some(Ok(PyRep::keyval([
                ("systemID", PyRep::Integer(sys_id as i64)),
                ("wormholeClass", PyRep::Integer(wh_class as i64)),
                ("isThera", PyRep::Bool(is_thera)),
                ("isShattered", PyRep::Bool(is_shattered)),
                ("delayedLocal", PyRep::Bool(delayed_local)),
                ("allowsCapitalShips", PyRep::Bool(allows_capitals)),
            ])))
        }

        ("scanMgr", "DirectionalScan") | ("scanMgr", "DScan") | ("directionalScanMgr", "PerformDirectionalScan") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let scan_angle = args.first().and_then(|a| a.as_f64()).unwrap_or(360.0).clamp(5.0, 360.0);
            let max_scan_range = 14.3 * AU_IN_METERS;
            let scan_range = args.get(1).and_then(|a| a.as_f64()).unwrap_or(max_scan_range).min(max_scan_range).max(0.0);

            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let caller_ship = session.ship_id;
            let caller_ball = if let Some(sid) = caller_ship {
                sys_handle.get_ball_snapshot(sid).await.ok().flatten().map(|(_, b)| b)
            } else {
                None
            };

            let caller_pos = caller_ball.as_ref().map(|b| b.position).unwrap_or(DVec3::ZERO);
            let caller_dir = caller_ball.as_ref().map(|b| {
                if b.velocity.length_squared() > 0.001 {
                    b.velocity.normalize()
                } else {
                    DVec3::new(0.0, 0.0, 1.0)
                }
            }).unwrap_or(DVec3::new(0.0, 0.0, 1.0));

            let cos_half_angle = (scan_angle.to_radians() * 0.5).cos();

            let balls = sys_handle.list_balls().await.unwrap_or_default();
            let mut rows = Vec::new();
            for ball in balls {
                if Some(ball.id) == caller_ship {
                    continue;
                }
                let dist = ball.position.distance(caller_pos);
                if dist > scan_range {
                    continue;
                }
                if scan_angle < 360.0 {
                    let to_target = (ball.position - caller_pos).normalize_or_zero();
                    if to_target.dot(caller_dir) < cos_half_angle {
                        continue;
                    }
                }
                rows.push(vec![
                    PyRep::Integer(ball.id),
                    PyRep::Integer(ball.type_id as i64),
                    PyRep::Real(dist),
                    PyRep::Real(ball.position.x),
                    PyRep::Real(ball.position.y),
                    PyRep::Real(ball.position.z),
                ]);
            }

            Some(Ok(PyRep::rowset(vec!["ballID", "typeID", "distance", "x", "y", "z"], rows)))
        }

        ("scanMgr", "LaunchProbes") | ("probeMgr", "LaunchProbes") => {
            let (_, _) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let probe_count = args.first().and_then(|a| a.as_i64()).unwrap_or(8).clamp(1, 8);
            Some(Ok(PyRep::keyval([
                ("success", PyRep::Bool(true)),
                ("probeCount", PyRep::Integer(probe_count)),
                ("scanRadiusMeters", PyRep::Real(4.0 * AU_IN_METERS)),
            ])))
        }

        ("scanMgr", "RecallProbes") | ("probeMgr", "RecallProbes") => {
            let (_, _) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::keyval([
                ("success", PyRep::Bool(true)),
                ("probesRecalled", PyRep::Integer(8)),
            ])))
        }

        ("scanMgr", "DestroyProbes") | ("probeMgr", "DestroyProbes") => {
            let (_, _) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::keyval([
                ("success", PyRep::Bool(true)),
            ])))
        }

        ("keeper", "ActivateAccelerationGate")
        | ("beyonce", "ActivateAccelerationGate")
        | ("dungeonMgr", "ActivateAccelerationGate")
        | ("accelerationGate", "Activate") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let gate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let Some((_, ship_ball)) = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: "Ship is not in space".into(),
                }));
            };

            let ship_type_id = ship_ball.type_id;
            let ship_group_id = ctx
                .sde
                .types
                .get(&ship_type_id)
                .map(|t| t.group_id)
                .unwrap_or(0);

            // Capital ship restriction from SDE ship group or mass
            if [30, 485, 547, 659, 883, 1538, 4594].contains(&ship_group_id)
                || ship_ball.mass > 500_000_000.0
            {
                return Some(Err(BridgeRpcError {
                    code: "ShipTooLargeForGate".into(),
                    message: "Capital ships cannot activate this acceleration gate".into(),
                }));
            }

            // Gate distance check if gate ball exists
            if gate_id > 0 {
                if let Ok(Some((_, gate_ball))) = sys_handle.get_ball_snapshot(gate_id).await {
                    let dist = ship_ball.position.distance(gate_ball.position);
                    if dist > 2_500.0 {
                        return Some(Err(BridgeRpcError {
                            code: "OutsideGateActivationRange".into(),
                            message: format!("Ship is {:.1} m from gate (must be <= 2500 m)", dist),
                        }));
                    }
                }
            }

            // Per-gate SDE dungeon ship class restriction & key item check
            let gate_check = ctx.db.try_with_mut_snapshot(|snap| {
                let (dungeon_id_opt, req_key_opt, consume_key) = snap
                    .items
                    .get(&gate_id)
                    .map(|g| {
                        let did = g.dynamic_attributes.get(&1001).map(|v| *v as u32);
                        let key_tid = g.dynamic_attributes.get(&1002).map(|v| *v as u32);
                        let consume = g.dynamic_attributes.get(&1003).is_some_and(|v| *v >= 0.5);
                        (did, key_tid, consume)
                    })
                    .unwrap_or((None, None, false));

                let effective_dungeon_id = dungeon_id_opt.or_else(|| {
                    args.get(1)
                        .and_then(|a| a.as_i64())
                        .and_then(|v| u32::try_from(v).ok())
                });

                if let Some(did) = effective_dungeon_id {
                    if let Some(dungeon) = ctx.sde.dungeons.get(&did) {
                        match dungeon.difficulty {
                            Some(1) => {
                                const ALLOWED_SCOUT_GROUPS: &[u32] = &[
                                    25, 31, 324, 420, 541, 830, 831, 834, 1283, 1305, 1527, 1534,
                                ];
                                if ship_group_id > 0 && !ALLOWED_SCOUT_GROUPS.contains(&ship_group_id) {
                                    return Err(BridgeRpcError {
                                        code: "ShipRestrictedByDungeonGate".into(),
                                        message: format!(
                                            "Dungeon '{}' (ID {did}, difficulty 1) restricts gate {gate_id} to Frigate/Destroyer hulls (ship group {ship_group_id} rejected)",
                                            dungeon.name
                                        ),
                                    });
                                }
                            }
                            Some(2) | Some(3) => {
                                if [27, 898, 900].contains(&ship_group_id) {
                                    return Err(BridgeRpcError {
                                        code: "ShipRestrictedByDungeonGate".into(),
                                        message: format!(
                                            "Dungeon '{}' (ID {did}) restricts gate {gate_id} against Battleship hulls",
                                            dungeon.name
                                        ),
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }

                let mut key_consumed = false;
                if let Some(req_key_type_id) = req_key_opt.filter(|&k| k > 0) {
                    let key_item_id = snap
                        .items
                        .values()
                        .find(|i| {
                            i.location_id == ship_id
                                && i.flag == eve_db::InventoryFlag::Cargo
                                && i.type_id == req_key_type_id
                                && i.quantity >= 1
                        })
                        .map(|i| i.item_id);

                    let Some(kid) = key_item_id else {
                        return Err(BridgeRpcError {
                            code: "MissingGateKey".into(),
                            message: format!(
                                "Acceleration gate {gate_id} requires key item type {req_key_type_id} in active ship cargo"
                            ),
                        });
                    };

                    if consume_key {
                        if let Some(item) = snap.items.get_mut(&kid) {
                            item.quantity -= 1;
                            if item.quantity <= 0 {
                                snap.items.remove(&kid);
                            }
                            key_consumed = true;
                        }
                    }
                }

                Ok((effective_dungeon_id, req_key_opt, key_consumed))
            });

            let (dungeon_id_used, key_type_used, key_consumed) = match gate_check {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            // Warps ship forward into the dungeon pocket (500 km >= 150 km min warp distance)
            let pocket_pos = ship_ball.position + DVec3::new(500_000.0, 0.0, 0.0);
            let warp_mode = MovementMode::new_warp(ship_ball.position, pocket_pos, 3.0);
            let _ = sys_handle.set_movement_mode(ship_id, warp_mode).await;

            Some(Ok(PyRep::keyval([
                ("success", PyRep::Bool(true)),
                ("gateID", PyRep::Integer(gate_id)),
                ("dungeonID", dungeon_id_used.map(|d| PyRep::Integer(d as i64)).unwrap_or(PyRep::None)),
                ("requiredKeyTypeID", key_type_used.map(|k| PyRep::Integer(k as i64)).unwrap_or(PyRep::None)),
                ("keyConsumed", PyRep::Bool(key_consumed)),
                ("destinationX", PyRep::Real(pocket_pos.x)),
                ("destinationY", PyRep::Real(pocket_pos.y)),
                ("destinationZ", PyRep::Real(pocket_pos.z)),
            ])))
        }

        ("scanMgr", "GetGasSiteDetails") => {
            let Some(sig_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetGasSiteDetails requires signatureID".into(),
                }));
            };
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            let site = ctx.db.with_snapshot(|snap| {
                snap.cosmic_signatures.get(&sys_id).and_then(|sigs| {
                    sigs.iter().find(|s| s.signature_id == sig_id && s.sig_group == "Gas Site").cloned()
                })
            });
            let Some(sig) = site else {
                return Some(Err(BridgeRpcError {
                    code: "GasSiteNotFound".into(),
                    message: format!("Gas site {sig_id} not found in system {sys_id}"),
                }));
            };

            Some(Ok(PyRep::keyval([
                ("signatureID", PyRep::String(sig.signature_id)),
                ("gasTypeID", PyRep::Integer(25268)), // Amber Cytoserocin
                ("gasName", PyRep::String("Amber Cytoserocin".into())),
                ("cloudCount", PyRep::Integer(2)),
                ("cloudUnits", PyRep::Integer(3_000)),
                ("signalStrength", PyRep::Real(sig.signal_strength)),
                ("npcDelaySeconds", PyRep::Integer(900)),
                ("npcSpawned", PyRep::Bool(false)),
            ])))
        }

        ("scanMgr", "GetOreSiteDetails") => {
            let Some(sig_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetOreSiteDetails requires signatureID".into(),
                }));
            };
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            let site = ctx.db.with_snapshot(|snap| {
                snap.cosmic_signatures.get(&sys_id).and_then(|sigs| {
                    sigs.iter().find(|s| s.signature_id == sig_id && s.sig_group == "Ore Site").cloned()
                })
            });
            let Some(sig) = site else {
                return Some(Err(BridgeRpcError {
                    code: "OreSiteNotFound".into(),
                    message: format!("Ore site {sig_id} not found in system {sys_id}"),
                }));
            };

            let rare_types = vec![PyRep::Integer(1226), PyRep::Integer(1231), PyRep::Integer(21)]; // Jaspet, Hemorphite, Hedbergite

            Some(Ok(PyRep::keyval([
                ("signatureID", PyRep::String(sig.signature_id)),
                ("isRareSignature", PyRep::Bool(true)),
                ("signalStrength", PyRep::Real(sig.signal_strength)),
                ("rareOreTypes", PyRep::List(rare_types)),
            ])))
        }

        ("scanMgr", "GetSystemAnomalies")
        | ("scanMgr", "GetAnomalies")
        | ("probeMgr", "GetSystemAnomalies") => {
            let sys_id = args.first().and_then(|a| a.as_i64()).map(|v| v as u32).or(session.solar_system_id).unwrap_or(30_000_142);
            let now = ctx.now_unix();

            let anomalies = ctx.db.try_with_mut_snapshot(|snap| {
                let list = snap.cosmic_anomalies.entry(sys_id).or_insert_with(|| {
                    vec![
                        eve_db::CosmicAnomalyRecord {
                            anomaly_id: format!("ANO-{}", 101 + (sys_id % 400)),
                            solar_system_id: sys_id,
                            name: "Small Asteroid Cluster".into(),
                            anomaly_type: "Ore".into(),
                            position: [2.0 * AU_IN_METERS, 1.0 * AU_IN_METERS, 0.5 * AU_IN_METERS],
                            signal_strength: 1.0,
                            ore_types: vec![1230, 1228], // Veldspar, Scordite
                            remaining_volume: 50_000.0,
                            created_unix: now,
                            lifetime_seconds: 86_400,
                            cleared: false,
                        },
                        eve_db::CosmicAnomalyRecord {
                            anomaly_id: format!("ANO-{}", 201 + (sys_id % 300)),
                            solar_system_id: sys_id,
                            name: "Medium Asteroid Cluster".into(),
                            anomaly_type: "Ore".into(),
                            position: [-4.0 * AU_IN_METERS, 3.0 * AU_IN_METERS, 1.5 * AU_IN_METERS],
                            signal_strength: 1.0,
                            ore_types: vec![1230, 1228, 1224], // Veldspar, Scordite, Pyroxeres
                            remaining_volume: 120_000.0,
                            created_unix: now,
                            lifetime_seconds: 86_400,
                            cleared: false,
                        },
                    ]
                });
                let active: Vec<eve_db::CosmicAnomalyRecord> = list.iter().filter(|a| !a.cleared).cloned().collect();
                Ok::<_, std::convert::Infallible>(active)
            }).unwrap();

            let rows: Vec<Vec<PyRep>> = anomalies.into_iter().map(|a| {
                let ores: Vec<PyRep> = a.ore_types.into_iter().map(|t| PyRep::Integer(t as i64)).collect();
                vec![
                    PyRep::String(a.anomaly_id),
                    PyRep::String(a.name),
                    PyRep::String(a.anomaly_type),
                    PyRep::Integer(a.solar_system_id as i64),
                    PyRep::Real(a.signal_strength),
                    PyRep::List(ores),
                    PyRep::Real(a.remaining_volume),
                    PyRep::Bool(a.cleared),
                ]
            }).collect();

            Some(Ok(PyRep::rowset(
                vec![
                    "anomalyID",
                    "name",
                    "anomalyType",
                    "solarSystemID",
                    "signalStrength",
                    "oreTypes",
                    "remainingVolume",
                    "cleared",
                ],
                rows,
            )))
        }

        ("scanMgr", "ClearAnomaly") => {
            let Some(ano_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "ClearAnomaly requires anomalyID".into(),
                }));
            };
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            let cleared = ctx.db.with_mut_snapshot(|snap| {
                if let Some(list) = snap.cosmic_anomalies.get_mut(&sys_id) {
                    if let Some(a) = list.iter_mut().find(|a| a.anomaly_id == ano_id) {
                        a.cleared = true;
                        return true;
                    }
                }
                false
            });

            Some(Ok(PyRep::keyval([
                ("anomalyID", PyRep::String(ano_id.into())),
                ("cleared", PyRep::Bool(cleared)),
                ("despawned", PyRep::Bool(cleared)),
            ])))
        }

        ("sovMgr", "InstallProspectingUpgrade") => {
            let sys_id = args.first().and_then(|a| a.as_i64()).map(|v| v as u32).or(session.solar_system_id).unwrap_or(30_000_142);
            let level = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1) as u32;
            let now = ctx.now_unix();

            let sites_spawned = ctx.db.with_mut_snapshot(|snap| {
                snap.sov_prospecting_levels.insert(sys_id, level);
                let list = snap.cosmic_anomalies.entry(sys_id).or_default();
                let mut count = 0;
                if level >= 1 && !list.iter().any(|a| a.name == "Enormous Asteroid Cluster") {
                    list.push(eve_db::CosmicAnomalyRecord {
                        anomaly_id: format!("SOV-ORE-{}", 701 + (sys_id % 200)),
                        solar_system_id: sys_id,
                        name: "Enormous Asteroid Cluster".into(),
                        anomaly_type: "SovOre".into(),
                        position: [6.0 * AU_IN_METERS, -2.0 * AU_IN_METERS, 3.0 * AU_IN_METERS],
                        signal_strength: 1.0,
                        ore_types: vec![19, 1229], // Spodumain, Gneiss
                        remaining_volume: 500_000.0,
                        created_unix: now,
                        lifetime_seconds: 86_400 * 3,
                        cleared: false,
                    });
                    count += 1;
                }
                if level >= 2 && !list.iter().any(|a| a.name == "Colossal Asteroid Cluster") {
                    list.push(eve_db::CosmicAnomalyRecord {
                        anomaly_id: format!("SOV-ORE-{}", 801 + (sys_id % 200)),
                        solar_system_id: sys_id,
                        name: "Colossal Asteroid Cluster".into(),
                        anomaly_type: "SovOre".into(),
                        position: [-7.0 * AU_IN_METERS, 5.0 * AU_IN_METERS, -4.0 * AU_IN_METERS],
                        signal_strength: 1.0,
                        ore_types: vec![19, 22, 1223], // Spodumain, Arkonor, Bistot
                        remaining_volume: 1_200_000.0,
                        created_unix: now,
                        lifetime_seconds: 86_400 * 3,
                        cleared: false,
                    });
                    count += 1;
                }
                count
            });

            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("prospectingLevel", PyRep::Integer(level as i64)),
                ("sitesSpawned", PyRep::Integer(sites_spawned)),
            ])))
        }

        ("sovMgr", "GetSovOreSites") => {
            let sys_id = args.first().and_then(|a| a.as_i64()).map(|v| v as u32).or(session.solar_system_id).unwrap_or(30_000_142);
            let sites = ctx.db.with_snapshot(|snap| {
                snap.cosmic_anomalies.get(&sys_id).map(|list| {
                    list.iter().filter(|a| a.anomaly_type == "SovOre" && !a.cleared).cloned().collect::<Vec<_>>()
                }).unwrap_or_default()
            });

            let rows: Vec<Vec<PyRep>> = sites.into_iter().map(|a| {
                let ores: Vec<PyRep> = a.ore_types.into_iter().map(|t| PyRep::Integer(t as i64)).collect();
                vec![
                    PyRep::String(a.anomaly_id),
                    PyRep::String(a.name),
                    PyRep::Integer(a.solar_system_id as i64),
                    PyRep::List(ores),
                    PyRep::Real(a.remaining_volume),
                ]
            }).collect();

            Some(Ok(PyRep::rowset(
                vec!["siteID", "siteName", "solarSystemID", "oreTypes", "remainingVolume"],
                rows,
            )))
        }

        ("livingWorld", "GetMiningOps") | ("miningDirector", "GetBeltMiningOps") => {
            let sys_id = args.first().and_then(|a| a.as_i64()).map(|v| v as u32).or(session.solar_system_id).unwrap_or(30_000_144);
            let convoys = ctx.living_world.list_convoys();
            let mining_ops: Vec<_> = convoys.into_iter().filter(|c| c.archetype == crate::living_world::NpcArchetype::MiningOp && c.current_system_id == sys_id).collect();

            let rows: Vec<Vec<PyRep>> = mining_ops.into_iter().map(|c| {
                vec![
                    PyRep::Integer(c.convoy_id),
                    PyRep::String("MiningOp".into()),
                    PyRep::Integer(c.ship_type_id as i64), // Venture miner
                    PyRep::Integer(648),                   // Badger hauler
                    PyRep::Integer(c.current_system_id as i64),
                    PyRep::Integer(c.cargo_type_id as i64),
                    PyRep::Integer(c.cargo_quantity as i64),
                    PyRep::Bool(c.destroyed),
                ]
            }).collect();

            Some(Ok(PyRep::rowset(
                vec![
                    "convoyID",
                    "archetype",
                    "minerShipTypeID",
                    "haulerShipTypeID",
                    "systemID",
                    "cargoTypeID",
                    "cargoQuantity",
                    "destroyed",
                ],
                rows,
            )))
        }

        ("livingWorld", "StepMiningOp") | ("miningDirector", "StepMiningOp") => {
            let Some(convoy_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "StepMiningOp requires convoyID".into(),
                }));
            };

            match ctx.living_world.step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde).await {
                Ok(res) => Some(Ok(PyRep::keyval([
                    ("convoyID", PyRep::Integer(res.convoy_id)),
                    ("systemID", PyRep::Integer(res.system_id as i64)),
                    ("asteroidID", PyRep::Integer(res.asteroid_id)),
                    ("oreTypeID", PyRep::Integer(res.ore_type_id as i64)),
                    ("minedUnits", PyRep::Integer(res.mined_units as i64)),
                    ("remainingAsteroidUnits", PyRep::Integer(res.remaining_asteroid_units as i64)),
                    ("convoyCargoUnits", PyRep::Integer(res.convoy_cargo_units as i64)),
                    ("hauledToStation", PyRep::Bool(res.hauled_to_station)),
                ]))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "NpcMiningOpFailed".into(),
                    message: e,
                })),
            }
        }

        _ => handle_bookmark_and_fleet_extras(ctx, session, service, method, args),
    }
}

fn handle_bookmark_and_fleet_extras(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service == "account" && method == "GetWalletDivisionsInfo" {
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let corp_id = args.first().and_then(|a| a.as_i64()).or(session.corporation_id).unwrap_or(0);
        let res = ctx.db.with_snapshot(|snap| {
            let Some(corp) = snap.corporations.get(&corp_id) else {
                return Err(BridgeRpcError {
                    code: "CorporationNotFound".into(),
                    message: format!("Corporation {corp_id} not found"),
                });
            };
            let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
            if char_id != corp.ceo_id
                && !corp_roles::has_role(caller_role, corp_roles::ROLE_ACCOUNTANT)
                && !corp_roles::has_role(caller_role, corp_roles::ROLE_JUNIOR_ACCOUNTANT)
            {
                return Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Viewing corporation wallet divisions requires Accountant role".into(),
                });
            }
            let rows: Vec<Vec<PyRep>> = corp
                .wallet_divisions
                .iter()
                .map(|div| {
                    vec![
                        PyRep::Integer(div.division_id as i64),
                        PyRep::String(div.name.clone()),
                        PyRep::Real(div.balance.to_wire_f64()),
                    ]
                })
                .collect();
            Ok(rows)
        });
        return match res {
            Ok(rows) => Some(Ok(PyRep::rowset(
                vec!["divisionID", "name", "balance"],
                rows,
            ))),
            Err(e) => Some(Err(e)),
        };
    }

    if service == "account" && method == "GiveCashFromCorpAccount" {
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let corp_id = match session.corporation_id {
            Some(c) => c,
            None => {
                return Some(Err(BridgeRpcError {
                    code: "NoCorporation".into(),
                    message: "Character has no active corporation".into(),
                }))
            }
        };
        let from_div = args.first().and_then(|a| a.as_i64()).unwrap_or(1) as u8;
        let to_div = args.get(1).and_then(|a| a.as_i64()).unwrap_or(2) as u8;
        let amount_isk = args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.0);
        if amount_isk <= 0.0 || !amount_isk.is_finite() {
            return Some(Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "Transfer amount must be positive and finite".into(),
            }));
        }
        let transfer_amount = IskCents::from_f64_isk(amount_isk);
        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                return Err(BridgeRpcError {
                    code: "CorporationNotFound".into(),
                    message: format!("Corporation {corp_id} not found"),
                });
            };
            let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
            if char_id != corp.ceo_id && !corp_roles::has_role(caller_role, corp_roles::ROLE_ACCOUNTANT) {
                return Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Transferring between corp wallet divisions requires Accountant role".into(),
                });
            }
            let from_pos = corp
                .wallet_divisions
                .iter()
                .position(|d| d.division_id == from_div)
                .ok_or_else(|| BridgeRpcError {
                    code: "DivisionNotFound".into(),
                    message: format!("Source wallet division {from_div} not found"),
                })?;
            let to_pos = corp
                .wallet_divisions
                .iter()
                .position(|d| d.division_id == to_div)
                .ok_or_else(|| BridgeRpcError {
                    code: "DivisionNotFound".into(),
                    message: format!("Target wallet division {to_div} not found"),
                })?;
            let source_bal = corp.wallet_divisions[from_pos].balance;
            let new_source = source_bal.checked_sub(transfer_amount).ok_or_else(|| BridgeRpcError {
                code: "InsufficientDivisionFunds".into(),
                message: "Insufficient funds in source wallet division".into(),
            })?;
            let new_target = corp.wallet_divisions[to_pos]
                .balance
                .checked_add(transfer_amount)
                .unwrap_or(corp.wallet_divisions[to_pos].balance);
            corp.wallet_divisions[from_pos].balance = new_source;
            corp.wallet_divisions[to_pos].balance = new_target;
            Ok(())
        });
        return match res {
            Ok(()) => Some(Ok(PyRep::keyval([
                ("fromDivisionID", PyRep::Integer(from_div as i64)),
                ("toDivisionID", PyRep::Integer(to_div as i64)),
                ("transferredISK", PyRep::Real(amount_isk)),
            ]))),
            Err(e) => Some(Err(e)),
        };
    }

    if service == "corpRegistry" && method == "InsertApplication" {
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let Some(corp_id) = args.first().and_then(|a| a.as_i64()) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "InsertApplication requires target corporationID".into(),
            }));
        };
        let message = args.get(1).and_then(|a| a.as_str()).unwrap_or("o7 requesting to join");
        let now_unix = ctx.now_unix();
        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                return Err(BridgeRpcError {
                    code: "CorporationNotFound".into(),
                    message: format!("Corporation {corp_id} not found"),
                });
            };
            if corp.members.contains(&char_id) {
                return Err(BridgeRpcError {
                    code: "AlreadyInCorporation".into(),
                    message: "Character is already a member of this corporation".into(),
                });
            }
            snap.next_entity_id += 1;
            let app_id = snap.next_entity_id;
            corp.applications.insert(
                app_id,
                CorpApplicationRecord {
                    application_id: app_id,
                    character_id: char_id,
                    corporation_id: corp_id,
                    message: message.to_string(),
                    created_unix: now_unix,
                    status: "Applied".into(),
                },
            );
            Ok(app_id)
        });
        return match res {
            Ok(app_id) => {
                ctx.notify(
                    NotificationTarget::Corporation(corp_id),
                    "OnCorporationApplicationChanged",
                    "corpid",
                    vec![
                        PyRep::Integer(corp_id),
                        PyRep::Integer(char_id),
                        PyRep::Integer(app_id),
                    ],
                );
                Some(Ok(PyRep::keyval([
                    ("applicationID", PyRep::Integer(app_id)),
                    ("status", PyRep::String("Applied".into())),
                ])))
            }
            Err(e) => Some(Err(e)),
        };
    }

    if service == "corpRegistry" && method == "UpdateApplicationOffer" {
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let corp_id = match session.corporation_id {
            Some(c) => c,
            None => {
                return Some(Err(BridgeRpcError {
                    code: "NoCorporation".into(),
                    message: "Character has no active corporation".into(),
                }))
            }
        };
        let Some(app_id) = args.first().and_then(|a| a.as_i64()) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "UpdateApplicationOffer requires applicationID".into(),
            }));
        };
        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                return Err(BridgeRpcError {
                    code: "CorporationNotFound".into(),
                    message: format!("Corporation {corp_id} not found"),
                });
            };
            let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
            if char_id != corp.ceo_id && !corp_roles::has_role(caller_role, corp_roles::ROLE_PERSONNEL_MANAGER) {
                return Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Accepting applications requires Personnel Manager or Director role".into(),
                });
            }
            let Some(app) = corp.applications.get_mut(&app_id) else {
                return Err(BridgeRpcError {
                    code: "ApplicationNotFound".into(),
                    message: format!("Application {app_id} not found"),
                });
            };
            let applicant_id = app.character_id;
            app.status = "Accepted".into();
            corp.members.insert(applicant_id);
            corp.member_roles.insert(applicant_id, 0);
            if let Some(ch) = snap.characters.get_mut(&applicant_id) {
                ch.corporation_id = corp_id;
                ch.alliance_id = corp.alliance_id;
            }
            Ok(applicant_id)
        });
        return match res {
            Ok(applicant_id) => {
                ctx.notify(
                    NotificationTarget::Character(applicant_id),
                    "OnCorporationApplicationChanged",
                    "charid",
                    vec![PyRep::Integer(corp_id), PyRep::Integer(app_id)],
                );
                Some(Ok(PyRep::keyval([
                    ("applicationID", PyRep::Integer(app_id)),
                    ("status", PyRep::String("Accepted".into())),
                    ("characterID", PyRep::Integer(applicant_id)),
                ])))
            }
            Err(e) => Some(Err(e)),
        };
    }

    if !matches!(service, "accessGroupBookmarkMgr" | "bookmark") {
        return None;
    }

    if method == "AddFolder" {
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let name = args.first().and_then(|a| a.as_str()).unwrap_or("New Folder");
        let is_shared = matches!(args.get(1), Some(PyRep::Bool(true)))
            || args.get(1).and_then(|a| a.as_i64()).is_some_and(|v| v > 0);
        if is_shared {
            let db_corp_id = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .filter(|cid| snap.corporations.contains_key(cid))
            });
            if let Some(cid) = db_corp_id {
                session.corporation_id = Some(cid);
            }
            let corp_id = match session.corporation_id {
                Some(c) => c,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NoCorporation".into(),
                        message: "Creating shared folder requires corporation membership".into(),
                    }))
                }
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let has_rights = if let Some(corp) = snap.corporations.get(&corp_id) {
                    let role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                    char_id == corp.ceo_id
                        || corp_roles::has_role(role, corp_roles::ROLE_DIRECTOR)
                        || corp_roles::has_role(role, corp_roles::ROLE_PERSONNEL_MANAGER)
                } else {
                    corp_roles::has_role(session.corp_role, corp_roles::ROLE_DIRECTOR)
                        || corp_roles::has_role(session.corp_role, corp_roles::ROLE_PERSONNEL_MANAGER)
                };
                if !has_rights {
                    return Err(BridgeRpcError {
                        code: "BookmarkAccessDenied".into(),
                        message: "Creating a shared bookmark folder requires corporation Director/CEO access-group rights".into(),
                    });
                }
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.bookmark_folders.insert(
                    id,
                    BookmarkFolderRecord {
                        folder_id: id,
                        owner_id: char_id,
                        folder_name: name.to_string(),
                        is_shared: true,
                        corporation_id: Some(corp_id),
                    },
                );
                Ok(id)
            });
            return match res {
                Ok(folder_id) => {
                    ctx.notify(
                        NotificationTarget::Corporation(corp_id),
                        "OnSharedBookmarksFolderUpdated",
                        "corpid",
                        vec![PyRep::Integer(folder_id), PyRep::Integer(corp_id)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("folderID", PyRep::Integer(folder_id)),
                        ("folderName", PyRep::String(name.to_string())),
                        ("isShared", PyRep::Bool(true)),
                        ("corporationID", PyRep::Integer(corp_id)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            };
        }

        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let personal_root_count = snap
                .bookmark_folders
                .values()
                .filter(|f| f.owner_id == char_id && !f.is_shared && !f.folder_name.contains('/'))
                .count();
            if personal_root_count >= 5 {
                return Err(BridgeRpcError {
                    code: "MaxPersonalFoldersReached".into(),
                    message: "Character has reached the maximum of 5 personal bookmark folders".into(),
                });
            }
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.bookmark_folders.insert(
                id,
                BookmarkFolderRecord {
                    folder_id: id,
                    owner_id: char_id,
                    folder_name: name.to_string(),
                    is_shared: false,
                    corporation_id: None,
                },
            );
            Ok(id)
        });
        return match res {
            Ok(folder_id) => Some(Ok(PyRep::keyval([
                ("folderID", PyRep::Integer(folder_id)),
                ("folderName", PyRep::String(name.to_string())),
                ("isShared", PyRep::Bool(false)),
            ]))),
            Err(e) => Some(Err(e)),
        };
    }

    if method == "DeleteFolder" {
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let Some(folder_id) = args.first().and_then(|a| a.as_i64()) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "DeleteFolder requires folderID".into(),
            }));
        };
        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let Some(f) = snap.bookmark_folders.get(&folder_id) else {
                return Err(BridgeRpcError {
                    code: "FolderNotFound".into(),
                    message: format!("Folder {folder_id} not found"),
                });
            };
            if f.owner_id != char_id {
                return Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "Cannot delete another character's folder".into(),
                });
            }
            snap.bookmark_folders.remove(&folder_id);
            for bm in snap.bookmarks.values_mut() {
                if bm.folder_id == Some(folder_id) {
                    bm.folder_id = None;
                }
            }
            Ok(())
        });
        return match res {
            Ok(()) => Some(Ok(PyRep::keyval([
                ("folderID", PyRep::Integer(folder_id)),
                ("deleted", PyRep::Bool(true)),
            ]))),
            Err(e) => Some(Err(e)),
        };
    }

    if method == "CreateSubfolder" {
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let Some(parent_folder_id) = args.first().and_then(|a| a.as_i64()) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "CreateSubfolder requires parent folderID".into(),
            }));
        };
        let sub_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("Subfolder");

        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let Some(parent) = snap.bookmark_folders.get(&parent_folder_id).cloned() else {
                return Err(BridgeRpcError {
                    code: "FolderNotFound".into(),
                    message: format!("Parent folder {parent_folder_id} not found"),
                });
            };
            let prefix = format!("{}/", parent.folder_name);
            let sub_count = snap
                .bookmark_folders
                .values()
                .filter(|f| f.owner_id == char_id && f.folder_name.starts_with(&prefix))
                .count();
            if sub_count >= 5 {
                return Err(BridgeRpcError {
                    code: "MaxSubfoldersReached".into(),
                    message: "Maximum of 5 subfolders per folder reached".into(),
                });
            }
            let full_name = format!("{}/{sub_name}", parent.folder_name);
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.bookmark_folders.insert(
                id,
                BookmarkFolderRecord {
                    folder_id: id,
                    owner_id: char_id,
                    folder_name: full_name.clone(),
                    is_shared: parent.is_shared,
                    corporation_id: parent.corporation_id,
                },
            );
            Ok((id, full_name))
        });

        return match res {
            Ok((subfolder_id, full_name)) => Some(Ok(PyRep::keyval([
                ("subfolderID", PyRep::Integer(subfolder_id)),
                ("folderID", PyRep::Integer(subfolder_id)),
                ("parentFolderID", PyRep::Integer(parent_folder_id)),
                ("subfolderName", PyRep::String(sub_name.to_string())),
                ("folderName", PyRep::String(full_name)),
            ]))),
            Err(e) => Some(Err(e)),
        };
    }

    if method == "DeleteSubfolder" {
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let Some(subfolder_id) = args.first().and_then(|a| a.as_i64()) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "DeleteSubfolder requires subfolderID".into(),
            }));
        };
        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let Some(f) = snap.bookmark_folders.get(&subfolder_id) else {
                return Err(BridgeRpcError {
                    code: "SubfolderNotFound".into(),
                    message: format!("Subfolder {subfolder_id} not found"),
                });
            };
            if f.owner_id != char_id {
                return Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "Cannot delete another character's subfolder".into(),
                });
            }
            snap.bookmark_folders.remove(&subfolder_id);
            for bm in snap.bookmarks.values_mut() {
                if bm.folder_id == Some(subfolder_id) {
                    bm.folder_id = None;
                }
            }
            Ok(())
        });
        return match res {
            Ok(()) => Some(Ok(PyRep::keyval([
                ("subfolderID", PyRep::Integer(subfolder_id)),
                ("deleted", PyRep::Bool(true)),
            ]))),
            Err(e) => Some(Err(e)),
        };
    }

    if method == "AddToKnownFolders" {
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let Some(folder_id) = args.first().and_then(|a| a.as_i64()) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "AddToKnownFolders requires folderID".into(),
            }));
        };
        let db_corp_id = ctx.db.with_snapshot(|snap| {
            snap.characters
                .get(&char_id)
                .map(|c| c.corporation_id)
                .filter(|cid| snap.corporations.contains_key(cid))
        });
        if let Some(cid) = db_corp_id {
            session.corporation_id = Some(cid);
        }
        let check = ctx.db.with_snapshot(|snap| {
            let Some(f) = snap.bookmark_folders.get(&folder_id) else {
                return Err(BridgeRpcError {
                    code: "FolderNotFound".into(),
                    message: format!("Shared folder {folder_id} not found"),
                });
            };
            if !f.is_shared || f.corporation_id != session.corporation_id {
                return Err(BridgeRpcError {
                    code: "BookmarkAccessDenied".into(),
                    message: "Character lacks access-group rights to subscribe to this shared bookmark folder".into(),
                });
            }
            Ok((f.folder_name.clone(), f.corporation_id))
        });
        return match check {
            Ok((folder_name, corp_id_opt)) => {
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnSharedBookmarksFolderUpdated",
                    "charid",
                    vec![PyRep::Integer(folder_id), PyRep::Bool(true)],
                );
                if let Some(corp_id) = corp_id_opt {
                    ctx.notify(
                        NotificationTarget::Corporation(corp_id),
                        "OnSharedBookmarksFolderUpdated",
                        "corpid",
                        vec![PyRep::Integer(folder_id), PyRep::Integer(corp_id)],
                    );
                }
                Some(Ok(PyRep::keyval([
                    ("folderID", PyRep::Integer(folder_id)),
                    ("folderName", PyRep::String(folder_name)),
                    ("subscribed", PyRep::Bool(true)),
                ])))
            }
            Err(e) => Some(Err(e)),
        };
    }

    if method == "RemoveFromKnownFolders" {
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let Some(folder_id) = args.first().and_then(|a| a.as_i64()) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "RemoveFromKnownFolders requires folderID".into(),
            }));
        };
        let exists = ctx.db.with_snapshot(|snap| snap.bookmark_folders.contains_key(&folder_id));
        if !exists {
            return Some(Err(BridgeRpcError {
                code: "FolderNotFound".into(),
                message: format!("Shared folder {folder_id} not found"),
            }));
        }
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnSharedBookmarksFolderUpdated",
            "charid",
            vec![PyRep::Integer(folder_id), PyRep::Bool(false)],
        );
        return Some(Ok(PyRep::keyval([
            ("folderID", PyRep::Integer(folder_id)),
            ("subscribed", PyRep::Bool(false)),
        ])));
    }

    None
}
