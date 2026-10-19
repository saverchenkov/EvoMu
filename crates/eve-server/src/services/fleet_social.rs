//! `fleet_social` — Domain 10: Fleets, Corporations, Chat (LSC), Mail, Bookmarks, and Probing/Wormholes (`SO-1`..`SO-7`).

#![forbid(unsafe_code)]

use crate::router::cache_notifications::NotificationTarget;
use crate::router::ServerContext;
use eve_db::{
    corp_roles, BookmarkRecord, ChatChannelRecord, CorpApplicationRecord,
    CorpWalletDivisionRecord, CorporationRecord, CosmicSignatureRecord,
    FleetMemberRecord, FleetRecord, FleetRole, FleetSquadRecord,
    FleetWingRecord, IskCents, MailRecord,
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
    match (service, method) {
        // =========================================================================
        // SO-1: Fleet Manager (`fleetMgr`, `fleetObjectHandler`, `fleetProxy`)
        // =========================================================================
        ("fleetMgr", "CreateFleet") | ("fleetObjectHandler", "CreateFleet") | ("fleetProxy", "CreateFleet") => {
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
                        NotificationTarget::Fleet(fid),
                        "OnFleetMemberLeft",
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

        ("fleetMgr", "LeaveFleet") | ("fleetObjectHandler", "LeaveFleet") => {
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
            let (_, _) = match ServerContext::require_character(session) {
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
                return Some(Err(BridgeRpcError {
                    code: "FleetNotFound".into(),
                    message: format!("Fleet {fid} not found"),
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
                let rows: Vec<Vec<PyRep>> = corp.wallet_divisions.iter().map(|div| {
                    vec![
                        PyRep::Integer(div.division_id as i64),
                        PyRep::String(div.name.clone()),
                        PyRep::Real(div.balance.to_wire_f64()),
                    ]
                }).collect();
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

        // =========================================================================
        // SO-3: Chat System (`lscService`, `lsc`, `chat`)
        // =========================================================================
        ("lscService", "JoinChannel") | ("lsc", "JoinChannel") | ("chat", "JoinChannel") => {
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

        ("lscService", "LeaveChannel") | ("lsc", "LeaveChannel") | ("chat", "LeaveChannel") => {
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

        ("lscService", "SendMessage") | ("lsc", "SendMessage") | ("chat", "SendMessage") => {
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

        ("lscService", "StartConversation") | ("lsc", "SendPrivateMessage") | ("chat", "StartConversation") => {
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

        ("lscService", "GetChannelInfo") | ("lsc", "GetChannelMembers") => {
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
                ("members", PyRep::rowset(vec!["characterID", "channelID"], rows)),
            ])))
        }

        // =========================================================================
        // SO-4: In-Game Mail (`mailMgr`)
        // =========================================================================
        ("mailMgr", "SendMail") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(recipient_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidRecipient".into(),
                    message: "SendMail requires recipientID".into(),
                }));
            };
            let subject = args.get(1).and_then(|a| a.as_str()).unwrap_or("o7");
            let body = args.get(2).and_then(|a| a.as_str()).unwrap_or("Fly safe!");

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if snap.mails.len() >= 1_000 {
                    return Err(BridgeRpcError {
                        code: "MailQuotaExceeded".into(),
                        message: "Server mail spool limit (1,000 messages) reached".into(),
                    });
                }
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.mails.push(MailRecord {
                    message_id: id,
                    sender_id: char_id,
                    recipient_id,
                    subject: subject.to_string(),
                    body: body.to_string(),
                });
                Ok(id)
            });

            match res {
                Ok(mid) => {
                    ctx.notify(
                        NotificationTarget::Character(recipient_id),
                        "OnMailReceived",
                        "charid",
                        vec![PyRep::Integer(mid), PyRep::Integer(char_id)],
                    );
                    Some(Ok(PyRep::Integer(mid)))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("mailMgr", "GetHeaders") | ("mailMgr", "GetMailHeaders") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let rows = ctx.db.with_snapshot(|snap| {
                snap.mails
                    .iter()
                    .filter(|m| {
                        (m.recipient_id == char_id || m.sender_id == char_id)
                            && !snap.deleted_mails.contains(&(char_id, m.message_id))
                    })
                    .map(|m| {
                        let read = snap.read_mails.contains(&(char_id, m.message_id));
                        vec![
                            PyRep::Integer(m.message_id),
                            PyRep::Integer(m.sender_id),
                            PyRep::List(vec![PyRep::Integer(m.recipient_id)]),
                            PyRep::String(m.subject.clone()),
                            PyRep::Integer(133_000_000_000_000_000), // Standard BlueOS filetime
                            PyRep::Bool(read),
                        ]
                    })
                    .collect()
            });

            Some(Ok(PyRep::rowset(
                vec!["messageID", "senderID", "toCharacterIDs", "title", "sentDate", "read"],
                rows,
            )))
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

            let mail_opt = ctx.db.with_snapshot(|snap| {
                snap.mails.iter().find(|m| m.message_id == mail_id).cloned()
            });

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
            ])))
        }

        ("mailMgr", "MarkRead") | ("mailMgr", "MarkMailRead") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(mail_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "MarkRead requires messageID".into(),
                }));
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                snap.read_mails.insert((char_id, mail_id));
                Ok::<(), std::convert::Infallible>(())
            }).unwrap();

            Some(Ok(PyRep::keyval([
                ("messageID", PyRep::Integer(mail_id)),
                ("read", PyRep::Bool(true)),
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
        // SO-5: Bookmarks (`bookmark`, `accessGroupBookmarkMgr`)
        // =========================================================================
        ("bookmark", "BookmarkLocation") | ("accessGroupBookmarkMgr", "BookmarkStaticLocation") | ("bookmark", "BookmarkEntity") => {
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
            let explicit_entity_id = args.get(1).and_then(|a| a.as_i64());
            let memo = args.get(2).and_then(|a| a.as_str()).unwrap_or("").to_string();

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
                    },
                );
                Ok(id)
            });

            match res {
                Ok(bm_id) => Some(Ok(PyRep::keyval([
                    ("bookmarkID", PyRep::Integer(bm_id)),
                    ("label", PyRep::String(label.to_string())),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "GetBookmarks") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.bookmarks
                    .values()
                    .filter(|b| b.owner_id == char_id)
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
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["bookmarkID", "label", "solarSystemID", "x", "y", "z", "itemID", "memo"],
                rows,
            )))
        }

        ("bookmark", "DeleteBookmark") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(bm_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DeleteBookmark requires bookmarkID".into(),
                }));
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(bm) = snap.bookmarks.get(&bm_id) else {
                    return Err(BridgeRpcError {
                        code: "BookmarkNotFound".into(),
                        message: format!("Bookmark {bm_id} not found"),
                    });
                };
                if bm.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Cannot delete another character's bookmark".into(),
                    });
                }
                snap.bookmarks.remove(&bm_id);
                Ok(())
            });

            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("bookmarkID", PyRep::Integer(bm_id)),
                    ("deleted", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
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
                let list = snap.cosmic_signatures.entry(sys_id).or_insert_with(|| {
                    vec![
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
                    ]
                });
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

        ("wormholeMgr", "JumpWormhole") => {
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

                let dest_sys = wh.destination_system_id.unwrap_or(31_000_005);
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

        _ => None,
    }
}
