//! Protocol-agnostic fleet domain service (`#593` Domain 1).
//!
//! Shared across MachoNet (`fleetMgr`, `fleetObjectHandler`, `fleetProxy`) and ESI (`/fleets/*`,
//! `/characters/{character_id}/fleet`).

use super::DomainError;
use crate::router::ServerContext;
use eve_db::{
    ChatChannelRecord, FleetMemberRecord, FleetRecord, FleetRole, FleetSquadRecord,
    FleetWingRecord,
};
use std::collections::{HashMap, HashSet};

pub const FLEET_JOB_CREATOR: u32 = 2;

pub fn role_to_i64(role: FleetRole) -> i64 {
    match role {
        FleetRole::FleetCommander => 1,
        FleetRole::WingCommander => 2,
        FleetRole::SquadCommander => 3,
        FleetRole::SquadMember => 4,
    }
}

pub fn i64_to_role(val: i64) -> FleetRole {
    match val {
        1 => FleetRole::FleetCommander,
        2 => FleetRole::WingCommander,
        3 => FleetRole::SquadCommander,
        _ => FleetRole::SquadMember,
    }
}

pub fn role_name_str(role: FleetRole) -> &'static str {
    match role {
        FleetRole::FleetCommander => "Fleet Commander",
        FleetRole::WingCommander => "Wing Commander",
        FleetRole::SquadCommander => "Squad Commander",
        FleetRole::SquadMember => "Squad Member",
    }
}

pub fn fleet_role_to_esi_str(role: FleetRole) -> (&'static str, &'static str) {
    match role {
        FleetRole::FleetCommander => ("fleet_commander", "Fleet Commander"),
        FleetRole::WingCommander => ("wing_commander", "Wing Commander"),
        FleetRole::SquadCommander => ("squad_commander", "Squad Commander"),
        FleetRole::SquadMember => ("squad_member", "Squad Member"),
    }
}

pub fn parse_esi_fleet_role(role: &str) -> Option<FleetRole> {
    match role {
        "fleet_commander" => Some(FleetRole::FleetCommander),
        "wing_commander" => Some(FleetRole::WingCommander),
        "squad_commander" => Some(FleetRole::SquadCommander),
        "squad_member" => Some(FleetRole::SquadMember),
        _ => None,
    }
}

pub fn ensure_fleet_channel_member(
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

pub fn is_fleet_member(f: &FleetRecord, char_id: i64) -> bool {
    f.boss_id == char_id || f.members.contains(&char_id) || f.member_details.contains_key(&char_id)
}

pub fn is_fleet_boss_or_fc(f: &FleetRecord, char_id: i64) -> bool {
    f.boss_id == char_id
        || f.member_details
            .get(&char_id)
            .is_some_and(|m| m.role == FleetRole::FleetCommander)
}

pub fn squad_parent_wing_id(f: &FleetRecord, squad_id: i64) -> Option<i64> {
    f.squad_details
        .get(&squad_id)
        .map(|s| s.wing_id)
        .or_else(|| {
            f.wing_details
                .iter()
                .find(|(_, w)| w.squad_ids.contains(&squad_id))
                .map(|(wid, _)| *wid)
        })
}

pub fn can_manage_wing(f: &FleetRecord, caller_id: i64, wing_id: i64) -> bool {
    is_fleet_boss_or_fc(f, caller_id)
        || f.member_details
            .get(&caller_id)
            .is_some_and(|m| m.role == FleetRole::WingCommander && m.wing_id == Some(wing_id))
}

pub fn can_manage_squad(f: &FleetRecord, caller_id: i64, squad_id: i64) -> bool {
    let wing_id = squad_parent_wing_id(f, squad_id);
    if is_fleet_boss_or_fc(f, caller_id) {
        return true;
    }
    f.member_details.get(&caller_id).is_some_and(|m| match m.role {
        FleetRole::FleetCommander => true,
        FleetRole::WingCommander => wing_id.is_some_and(|wid| m.wing_id == Some(wid)),
        FleetRole::SquadCommander => m.squad_id == Some(squad_id),
        FleetRole::SquadMember => false,
    })
}

pub fn member_effective_position(
    f: &FleetRecord,
    member_id: i64,
) -> (FleetRole, Option<i64>, Option<i64>) {
    if let Some(md) = f.member_details.get(&member_id) {
        (md.role, md.wing_id, md.squad_id)
    } else if f.boss_id == member_id {
        (FleetRole::FleetCommander, None, None)
    } else {
        (
            FleetRole::SquadMember,
            f.wings.first().copied(),
            f.squads.first().copied(),
        )
    }
}

pub fn can_act_on_member(
    f: &FleetRecord,
    caller_id: i64,
    target_id: i64,
    destination: Option<(FleetRole, Option<i64>, Option<i64>)>,
) -> bool {
    if target_id == f.boss_id && caller_id != f.boss_id {
        return false;
    }
    if caller_id == f.boss_id {
        return true;
    }
    let (target_role, target_wing, target_squad) = member_effective_position(f, target_id);
    let Some(caller_md) = f.member_details.get(&caller_id) else {
        return false;
    };
    let can_touch_source = match caller_md.role {
        FleetRole::FleetCommander => {
            target_role != FleetRole::FleetCommander || target_id == caller_id
        }
        FleetRole::WingCommander => {
            matches!(
                target_role,
                FleetRole::SquadCommander | FleetRole::SquadMember
            ) && caller_md.wing_id.is_some()
                && target_wing == caller_md.wing_id
        }
        FleetRole::SquadCommander => {
            target_role == FleetRole::SquadMember
                && caller_md.squad_id.is_some()
                && target_squad == caller_md.squad_id
        }
        FleetRole::SquadMember => false,
    };
    if !can_touch_source {
        return false;
    }
    if let Some((new_role, new_wing, new_squad)) = destination {
        match caller_md.role {
            FleetRole::FleetCommander => new_role != FleetRole::FleetCommander,
            FleetRole::WingCommander => {
                matches!(
                    new_role,
                    FleetRole::SquadCommander | FleetRole::SquadMember
                ) && new_wing == caller_md.wing_id
            }
            FleetRole::SquadCommander => {
                new_role == FleetRole::SquadMember
                    && new_squad == caller_md.squad_id
                    && (new_wing.is_none() || new_wing == caller_md.wing_id)
            }
            FleetRole::SquadMember => false,
        }
    } else {
        true
    }
}

pub fn can_invite_member(
    f: &FleetRecord,
    caller_id: i64,
    role: FleetRole,
    wing_id: Option<i64>,
    squad_id: Option<i64>,
) -> bool {
    if f.boss_id == caller_id {
        return true;
    }
    let Some(caller_md) = f.member_details.get(&caller_id) else {
        return false;
    };
    match caller_md.role {
        FleetRole::FleetCommander => role != FleetRole::FleetCommander,
        FleetRole::WingCommander => {
            matches!(role, FleetRole::SquadCommander | FleetRole::SquadMember)
                && caller_md.wing_id.is_some()
                && wing_id == caller_md.wing_id
        }
        FleetRole::SquadCommander => {
            role == FleetRole::SquadMember
                && caller_md.squad_id.is_some()
                && squad_id == caller_md.squad_id
                && (wing_id.is_none() || wing_id == caller_md.wing_id)
        }
        FleetRole::SquadMember => false,
    }
}

pub fn validate_wing_and_squad(
    f: &FleetRecord,
    fleet_id: i64,
    role: FleetRole,
    wing_id: Option<i64>,
    squad_id: Option<i64>,
    is_move: bool,
) -> Result<(Option<i64>, Option<i64>), DomainError> {
    if let Some(wid) = wing_id {
        if !f.wings.contains(&wid) && !f.wing_details.contains_key(&wid) {
            return Err(DomainError::not_found(
                "WingNotFound",
                format!("Wing {wid} not found in fleet {fleet_id}"),
            ));
        }
    }
    if let Some(sid) = squad_id {
        if !f.squads.contains(&sid) && !f.squad_details.contains_key(&sid) {
            return Err(DomainError::not_found(
                "SquadNotFound",
                format!("Squad {sid} not found in fleet {fleet_id}"),
            ));
        }
        if let Some(wid) = wing_id {
            if let Some(sq) = f.squad_details.get(&sid) {
                if sq.wing_id != wid {
                    return Err(DomainError::invalid_input(
                        "InvalidArgument",
                        format!("Squad {sid} does not belong to wing {wid} in fleet {fleet_id}"),
                    ));
                }
            } else if let Some(w) = f.wing_details.get(&wid) {
                if !w.squad_ids.is_empty() && !w.squad_ids.contains(&sid) {
                    return Err(DomainError::invalid_input(
                        "InvalidArgument",
                        format!("Squad {sid} does not belong to wing {wid} in fleet {fleet_id}"),
                    ));
                }
            }
        }
    }
    match role {
        FleetRole::FleetCommander => Ok((None, None)),
        FleetRole::WingCommander => {
            let resolved_wing =
                wing_id.or_else(|| if is_move { None } else { f.wings.first().copied() });
            if is_move && resolved_wing.is_none() {
                return Err(DomainError::invalid_input(
                    "InvalidArgument",
                    "Moving a member to wing_commander requires a valid wing_id",
                ));
            }
            Ok((resolved_wing, None))
        }
        FleetRole::SquadCommander | FleetRole::SquadMember => {
            let resolved_squad =
                squad_id.or_else(|| if is_move { None } else { f.squads.first().copied() });
            let resolved_wing = wing_id
                .or_else(|| resolved_squad.and_then(|s| squad_parent_wing_id(f, s)))
                .or_else(|| if is_move { None } else { f.wings.first().copied() });
            if is_move && (resolved_wing.is_none() || resolved_squad.is_none()) {
                return Err(DomainError::invalid_input(
                    "InvalidArgument",
                    "Moving a member to squad_commander or squad_member requires wing_id and squad_id",
                ));
            }
            Ok((resolved_wing, resolved_squad))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CharacterFleetMembership {
    pub fleet_id: i64,
    pub fleet_boss_id: i64,
    pub wing_id: Option<i64>,
    pub squad_id: Option<i64>,
    pub role: FleetRole,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetSettingsView {
    pub fleet_id: i64,
    pub motd: String,
    pub is_free_move: bool,
    pub is_registered: bool,
    pub is_voice_enabled: bool,
}

#[derive(Clone, Debug)]
pub struct UpdateFleetSettingsInput {
    pub fleet_id: i64,
    pub caller_char_id: Option<i64>,
    pub require_boss_only: bool,
    pub motd: Option<String>,
    pub is_free_move: Option<bool>,
    pub is_registered: Option<bool>,
    pub is_voice_enabled: Option<bool>,
    pub is_loot_logging: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct UpdateFleetSettingsOutcome {
    pub fleet_id: i64,
    pub old_free_move: bool,
    pub old_registered: bool,
    pub new_free_move: bool,
    pub new_registered: bool,
    pub auto_join_squad_id: Option<i64>,
    pub updated_motd: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetMemberEntry {
    pub character_id: i64,
    pub ship_type_id: i64,
    pub wing_id: i64,
    pub squad_id: i64,
    pub role: FleetRole,
    pub role_name: String,
    pub join_time_unix: u64,
    pub takes_fleet_warp: bool,
    pub solar_system_id: i64,
    pub station_id: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct InviteFleetMemberInput {
    pub fleet_id: i64,
    pub caller_char_id: Option<i64>,
    pub inviter_id_override: Option<i64>,
    pub invitee_id: i64,
    pub role: FleetRole,
    pub wing_id: Option<i64>,
    pub squad_id: Option<i64>,
    pub enforce_online_and_capacity_checks: bool,
    pub validate_hierarchy_strict: bool,
    pub now_unix: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InviteFleetMemberOutcome {
    pub fleet_id: i64,
    pub inviter_id: i64,
    pub invitee_id: i64,
    pub wing_id: i64,
    pub squad_id: i64,
    pub role: FleetRole,
    pub role_num: i64,
}

#[derive(Clone, Debug)]
pub struct AcceptFleetInviteInput {
    pub char_id: i64,
    pub fleet_hint: Option<i64>,
    pub now_unix: u64,
    pub current_solar_system_id: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptFleetInviteOutcome {
    pub fleet_id: i64,
    pub char_id: i64,
    pub wing_id: i64,
    pub squad_id: i64,
    pub role: FleetRole,
    pub motd: String,
    pub now_unix: u64,
    pub solar_system_id: u32,
}

#[derive(Clone, Debug)]
pub struct MoveFleetMemberInput {
    pub fleet_id: i64,
    pub caller_char_id: Option<i64>,
    pub target_id: i64,
    pub new_role: FleetRole,
    pub wing_id: Option<i64>,
    pub squad_id: Option<i64>,
    pub strict_esi_wing_squad: bool,
    pub allow_fallback_missing_wing_squad: bool,
    pub enforce_session_change_timer: bool,
    pub now_unix: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MoveFleetMemberOutcome {
    pub fleet_id: i64,
    pub target_id: i64,
    pub resolved_wing_id: Option<i64>,
    pub resolved_squad_id: Option<i64>,
    pub before: FleetMemberRecord,
    pub after: FleetMemberRecord,
    pub is_only_member: bool,
    pub self_moved: bool,
    pub now_unix: u64,
    pub target_solar_system_id: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RemoveFleetMemberOutcome {
    pub fleet_id: i64,
    pub removed_char_id: i64,
    pub was_member: bool,
    pub old_role_num: i64,
    pub disbanded: bool,
    pub handover: Option<(FleetMemberRecord, FleetMemberRecord, bool)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisbandFleetOutcome {
    pub fleet_id: i64,
    pub caller_id: i64,
    pub former_member_ids: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SquadSummary {
    pub squad_id: i64,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WingSummary {
    pub wing_id: i64,
    pub name: String,
    pub squads: Vec<SquadSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WingCreated {
    pub fleet_id: i64,
    pub wing_id: i64,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SquadCreated {
    pub fleet_id: i64,
    pub wing_id: i64,
    pub squad_id: i64,
    pub name: String,
}

pub fn get_character_fleet(
    ctx: &ServerContext,
    char_id: i64,
) -> Result<CharacterFleetMembership, DomainError> {
    ctx.db.with_snapshot(|snap| {
        if !snap.characters.contains_key(&char_id) {
            return Err(DomainError::not_found(
                "CharacterNotFound",
                format!("Character {char_id} not found"),
            ));
        }
        let Some(fleet) = snap.fleets.values().find(|f| is_fleet_member(f, char_id)) else {
            return Err(DomainError::not_found(
                "NotInFleet",
                format!("Character {char_id} is not in a fleet"),
            ));
        };
        let (wing_id, squad_id, role) = if let Some(md) = fleet.member_details.get(&char_id) {
            (md.wing_id, md.squad_id, md.role)
        } else if fleet.boss_id == char_id {
            (None, None, FleetRole::FleetCommander)
        } else {
            (
                fleet.wings.first().copied(),
                fleet.squads.first().copied(),
                FleetRole::SquadMember,
            )
        };
        Ok(CharacterFleetMembership {
            fleet_id: fleet.fleet_id,
            fleet_boss_id: fleet.boss_id,
            wing_id,
            squad_id,
            role,
        })
    })
}

pub fn get_fleet_settings(
    ctx: &ServerContext,
    fleet_id: i64,
    caller_char_id: Option<i64>,
) -> Result<FleetSettingsView, DomainError> {
    let is_voice_enabled = ctx
        .fleet_extras
        .lock()
        .unwrap()
        .voice_enabled
        .get(&fleet_id)
        .copied()
        .unwrap_or(false);
    ctx.db.with_snapshot(|snap| {
        let Some(f) = snap.fleets.get(&fleet_id) else {
            return Err(DomainError::not_found(
                "FleetNotFound",
                format!("Fleet {fleet_id} not found"),
            ));
        };
        if let Some(cid) = caller_char_id {
            if !is_fleet_member(f, cid) {
                return Err(DomainError::forbidden(
                    "NotInFleet",
                    format!("Character {cid} is not a member of fleet {fleet_id}"),
                ));
            }
        }
        Ok(FleetSettingsView {
            fleet_id,
            motd: f.motd.clone(),
            is_free_move: f.free_move,
            is_registered: f.is_registered,
            is_voice_enabled,
        })
    })
}

pub fn update_fleet_settings(
    ctx: &ServerContext,
    input: UpdateFleetSettingsInput,
) -> Result<UpdateFleetSettingsOutcome, DomainError> {
    if input.motd.is_none()
        && input.is_free_move.is_none()
        && input.is_registered.is_none()
        && input.is_voice_enabled.is_none()
        && input.is_loot_logging.is_none()
    {
        return Err(DomainError::invalid_input(
            "InvalidArgument",
            "Fleet update must specify 'motd' and/or fleet options",
        ));
    }

    let fleet_id = input.fleet_id;
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(f) = snap.fleets.get_mut(&fleet_id) else {
            return Err(DomainError::not_found(
                "FleetNotFound",
                format!("Fleet {fleet_id} not found"),
            ));
        };
        if let Some(cid) = input.caller_char_id {
            if !is_fleet_member(f, cid) {
                return Err(DomainError::forbidden(
                    "NotInFleet",
                    "Caller is not in fleet",
                ));
            }
            let allowed = if input.require_boss_only {
                f.boss_id == cid
            } else {
                is_fleet_boss_or_fc(f, cid)
            };
            if !allowed {
                return Err(DomainError::forbidden(
                    "FleetPermissionDenied",
                    format!(
                        "Character {cid} lacks fleet commander/boss permissions for fleet {fleet_id}"
                    ),
                ));
            }
        }
        let old_free = f.free_move;
        let old_reg = f.is_registered;
        if let Some(ref motd) = input.motd {
            f.motd = motd.clone();
        }
        if let Some(fm) = input.is_free_move {
            f.free_move = fm;
        }
        if let Some(reg) = input.is_registered {
            f.is_registered = reg;
        }
        Ok((old_free, old_reg, f.free_move, f.is_registered))
    })?;

    let auto_join_squad_id = {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        if let Some(ve) = input.is_voice_enabled {
            ex.voice_enabled.insert(fleet_id, ve);
        }
        if let Some(ll) = input.is_loot_logging {
            ex.loot_logging.insert(fleet_id, ll);
        }
        ex.auto_join_squads.get(&fleet_id).copied().flatten()
    };

    Ok(UpdateFleetSettingsOutcome {
        fleet_id,
        old_free_move: res.0,
        old_registered: res.1,
        new_free_move: res.2,
        new_registered: res.3,
        auto_join_squad_id,
        updated_motd: input.motd,
    })
}

pub fn list_fleet_members(
    ctx: &ServerContext,
    fleet_id: i64,
    caller_char_id: Option<i64>,
) -> Result<Vec<FleetMemberEntry>, DomainError> {
    ctx.db.with_snapshot(|snap| {
        let Some(f) = snap.fleets.get(&fleet_id) else {
            return Err(DomainError::not_found(
                "FleetNotFound",
                format!("Fleet {fleet_id} not found"),
            ));
        };
        if let Some(cid) = caller_char_id {
            if !is_fleet_member(f, cid) {
                return Err(DomainError::forbidden(
                    "NotInFleet",
                    format!("Character {cid} is not a member of fleet {fleet_id}"),
                ));
            }
        }
        let mut member_ids = f.members.clone();
        if !member_ids.contains(&f.boss_id) {
            member_ids.insert(0, f.boss_id);
        }
        let mut detail_ids: Vec<i64> = f.member_details.keys().copied().collect();
        detail_ids.sort_unstable();
        for cid in detail_ids {
            if !member_ids.contains(&cid) {
                member_ids.push(cid);
            }
        }
        let mut out = Vec::with_capacity(member_ids.len());
        for cid in member_ids {
            let Some(ch) = snap.characters.get(&cid) else {
                continue;
            };
            let is_boss = cid == f.boss_id;
            let (role, rname, wing_id, squad_id, takes_fleet_warp, join_time_unix) =
                if let Some(md) = f.member_details.get(&cid) {
                    let (_, default_rname) = fleet_role_to_esi_str(md.role);
                    let rname = if md.role_name.is_empty() {
                        default_rname.to_string()
                    } else {
                        md.role_name.clone()
                    };
                    (
                        md.role,
                        rname,
                        md.wing_id.unwrap_or(-1),
                        md.squad_id.unwrap_or(-1),
                        md.takes_fleet_warp,
                        md.join_time_unix,
                    )
                } else if is_boss {
                    (
                        FleetRole::FleetCommander,
                        "Fleet Commander".to_string(),
                        -1,
                        -1,
                        true,
                        0,
                    )
                } else {
                    (
                        FleetRole::SquadMember,
                        "Squad Member".to_string(),
                        -1,
                        -1,
                        true,
                        0,
                    )
                };
            let ship_type_id = snap
                .items
                .get(&ch.active_ship_id)
                .map(|i| i.type_id as i64)
                .unwrap_or(670);
            out.push(FleetMemberEntry {
                character_id: cid,
                ship_type_id,
                wing_id,
                squad_id,
                role,
                role_name: rname,
                join_time_unix,
                takes_fleet_warp,
                solar_system_id: ch.solar_system_id as i64,
                station_id: ch.station_id,
            });
        }
        Ok(out)
    })
}

pub fn invite_fleet_member(
    ctx: &ServerContext,
    input: InviteFleetMemberInput,
) -> Result<InviteFleetMemberOutcome, DomainError> {
    let fleet_id = input.fleet_id;
    let invitee_id = input.invitee_id;
    let (max_size, has_pending_ts) = {
        let ex = ctx.fleet_extras.lock().unwrap();
        (
            ex.max_sizes.get(&fleet_id).copied().unwrap_or(256),
            ex.invite_timestamps
                .keys()
                .any(|&(_, cid)| cid == invitee_id),
        )
    };

    let (target_online, target_in_fleet) = if input.enforce_online_and_capacity_checks {
        (
            crate::services::character_contacts::is_character_online(ctx, invitee_id),
            ctx.db.with_snapshot(|snap| {
                snap.fleets.values().any(|f| {
                    f.members.contains(&invitee_id) || f.member_details.contains_key(&invitee_id)
                })
            }),
        )
    } else {
        (true, false)
    };

    let (wid, sid, effective_inviter) = ctx.db.try_with_mut_snapshot(|snap| {
        let any_invite = snap
            .fleets
            .values()
            .any(|f| f.invites.contains(&invitee_id));
        let Some(f_ro) = snap.fleets.get(&fleet_id) else {
            return Err(DomainError::not_found(
                "FleetNotFound",
                format!("Fleet {fleet_id} not found"),
            ));
        };
        let (resolved_wing, resolved_squad) = if input.validate_hierarchy_strict {
            validate_wing_and_squad(
                f_ro,
                fleet_id,
                input.role,
                input.wing_id,
                input.squad_id,
                false,
            )?
        } else {
            (input.wing_id, input.squad_id)
        };
        if let Some(caller_id) = input.caller_char_id {
            if !can_invite_member(f_ro, caller_id, input.role, resolved_wing, resolved_squad) {
                if !is_fleet_member(f_ro, caller_id) {
                    return Err(DomainError::forbidden(
                        "NotInFleet",
                        "Caller is not in fleet",
                    ));
                }
                return Err(DomainError::forbidden(
                    "FleetPermissionDenied",
                    format!(
                        "Character {caller_id} lacks fleet commander/boss permissions for fleet {fleet_id}"
                    ),
                ));
            }
        }
        if !snap.characters.contains_key(&invitee_id) {
            return Err(DomainError::not_found(
                "CharacterNotFound",
                format!("Invited character {invitee_id} not found"),
            ));
        }

        let fleet = snap.fleets.get_mut(&fleet_id).unwrap();
        if input.enforce_online_and_capacity_checks {
            if fleet.members.len() >= max_size as usize {
                return Err(DomainError::precondition_failed(
                    "FleetFull",
                    format!("Fleet has reached its maximum size of {max_size}"),
                ));
            }
            if !target_online {
                return Err(DomainError::precondition_failed(
                    "CharacterOffline",
                    "Cannot invite an offline character to a fleet",
                ));
            }
            if target_in_fleet {
                return Err(DomainError::conflict(
                    "AlreadyInFleet",
                    "Target character is already in a fleet",
                ));
            }
            if has_pending_ts || any_invite {
                return Err(DomainError::conflict(
                    "InviteAlreadyPending",
                    "Target character already has a pending fleet invitation",
                ));
            }
        }

        let default_wing = fleet.wings.first().copied().unwrap_or(1);
        let wid = resolved_wing.unwrap_or(default_wing);
        let sid = resolved_squad.unwrap_or_else(|| {
            fleet
                .wing_details
                .get(&wid)
                .and_then(|w| w.squad_ids.first().copied())
                .or_else(|| fleet.squads.first().copied())
                .unwrap_or(1)
        });

        if input.enforce_online_and_capacity_checks {
            let pos_occupied = match input.role {
                FleetRole::FleetCommander => fleet
                    .member_details
                    .values()
                    .any(|m| m.role == FleetRole::FleetCommander),
                FleetRole::WingCommander => {
                    fleet
                        .wing_details
                        .get(&wid)
                        .and_then(|w| w.commander_id)
                        .is_some()
                        || fleet.member_details.values().any(|m| {
                            m.role == FleetRole::WingCommander && m.wing_id == Some(wid)
                        })
                }
                FleetRole::SquadCommander => {
                    fleet
                        .squad_details
                        .get(&sid)
                        .and_then(|s| s.commander_id)
                        .is_some()
                        || fleet.member_details.values().any(|m| {
                            m.role == FleetRole::SquadCommander && m.squad_id == Some(sid)
                        })
                }
                FleetRole::SquadMember => false,
            };
            if pos_occupied {
                return Err(DomainError::conflict(
                    "PositionOccupied",
                    "The requested fleet commander position is already occupied",
                ));
            }
        }

        let effective_inviter = input
            .inviter_id_override
            .or(input.caller_char_id)
            .filter(|&id| id > 0)
            .unwrap_or(fleet.boss_id);
        fleet.invites.insert(invitee_id);
        Ok((wid, sid, effective_inviter))
    })?;

    {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        ex.invite_details
            .insert((fleet_id, invitee_id), (wid, sid, input.role, effective_inviter));
        ex.invite_timestamps
            .insert((fleet_id, invitee_id), input.now_unix);
    }

    Ok(InviteFleetMemberOutcome {
        fleet_id,
        inviter_id: effective_inviter,
        invitee_id,
        wing_id: wid,
        squad_id: sid,
        role: input.role,
        role_num: role_to_i64(input.role),
    })
}

pub fn accept_fleet_invite(
    ctx: &ServerContext,
    input: AcceptFleetInviteInput,
) -> Result<AcceptFleetInviteOutcome, DomainError> {
    let char_id = input.char_id;
    let already_in_fleet = ctx.db.with_snapshot(|snap| {
        snap.fleets
            .values()
            .any(|f| f.members.contains(&char_id) || f.member_details.contains_key(&char_id))
    });
    if already_in_fleet {
        return Err(DomainError::conflict(
            "AlreadyInFleet",
            "Character is already in a fleet",
        ));
    }

    let fleet_id = ctx.db.with_snapshot(|snap| {
        if let Some(f) = input
            .fleet_hint
            .and_then(|h| snap.fleets.get(&h))
            .filter(|f| f.invites.contains(&char_id))
        {
            return f.fleet_id;
        }
        snap.fleets
            .values()
            .find(|f| f.invites.contains(&char_id))
            .map(|f| f.fleet_id)
            .unwrap_or(0)
    });
    if fleet_id == 0 {
        return Err(DomainError::not_found(
            "InviteExpired",
            "No active fleet invitation found or invitation expired",
        ));
    }
    if crate::services::space_destiny::check_session_change_timer_active(
        ctx,
        char_id,
        input.now_unix,
        None,
    )
    .is_some()
    {
        return Err(DomainError::precondition_failed(
            "SessionChangeTimerActive",
            "Session change timer is active",
        ));
    }

    let (max_size, auto_squad, invite_detail) = {
        let ex = ctx.fleet_extras.lock().unwrap();
        let ms = ex.max_sizes.get(&fleet_id).copied().unwrap_or(256);
        let asq = ex.auto_join_squads.get(&fleet_id).copied().flatten();
        let inv = ex.invite_details.get(&(fleet_id, char_id)).copied();
        (ms, asq, inv)
    };

    let (wid, sid, role, motd, char_sys_id) = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
            return Err(DomainError::not_found(
                "FleetNotFound",
                format!("Fleet {fleet_id} not found"),
            ));
        };
        if fleet.members.len() >= max_size as usize {
            return Err(DomainError::precondition_failed(
                "FleetFull",
                format!("Fleet has reached its maximum size of {max_size}"),
            ));
        }
        if !fleet.invites.remove(&char_id) {
            return Err(DomainError::not_found(
                "InviteExpired",
                "No active fleet invitation found",
            ));
        }
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
        let pos_occupied = match role {
            FleetRole::FleetCommander => fleet
                .member_details
                .values()
                .any(|m| m.role == FleetRole::FleetCommander),
            FleetRole::WingCommander => {
                fleet
                    .wing_details
                    .get(&wid)
                    .and_then(|w| w.commander_id)
                    .is_some()
                    || fleet
                        .member_details
                        .values()
                        .any(|m| m.role == FleetRole::WingCommander && m.wing_id == Some(wid))
            }
            FleetRole::SquadCommander => {
                fleet
                    .squad_details
                    .get(&sid)
                    .and_then(|s| s.commander_id)
                    .is_some()
                    || fleet
                        .member_details
                        .values()
                        .any(|m| m.role == FleetRole::SquadCommander && m.squad_id == Some(sid))
            }
            FleetRole::SquadMember => false,
        };
        if pos_occupied {
            return Err(DomainError::conflict(
                "PositionOccupied",
                "The invited fleet commander position is already occupied",
            ));
        }

        if role == FleetRole::WingCommander {
            if let Some(wing) = fleet.wing_details.get_mut(&wid) {
                wing.commander_id = Some(char_id);
            }
        }
        if role == FleetRole::SquadCommander {
            if let Some(sq) = fleet.squad_details.get_mut(&sid) {
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
                join_time_unix: input.now_unix,
            },
        );
        let motd = fleet.motd.clone();
        let boss_id = fleet.boss_id;
        ensure_fleet_channel_member(&mut snap.chat_channels, fleet_id, boss_id, char_id);
        let mut sys_id = 0_u32;
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.fleet_id = Some(fleet_id);
            sys_id = ch.solar_system_id;
        }
        Ok((wid, sid, role, motd, sys_id))
    })?;

    {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        ex.invite_details.remove(&(fleet_id, char_id));
        ex.invite_timestamps.remove(&(fleet_id, char_id));
        if let Some(reqs) = ex.join_requests.get_mut(&fleet_id) {
            reqs.remove(&char_id);
        }
    }

    let sys_id = input.current_solar_system_id.unwrap_or(char_sys_id);
    crate::services::space_destiny::record_session_change_timer(
        ctx,
        char_id,
        input.now_unix,
        sys_id,
        0,
        "fleet_change",
    );

    Ok(AcceptFleetInviteOutcome {
        fleet_id,
        char_id,
        wing_id: wid,
        squad_id: sid,
        role,
        motd,
        now_unix: input.now_unix,
        solar_system_id: sys_id,
    })
}

pub fn move_fleet_member(
    ctx: &ServerContext,
    input: MoveFleetMemberInput,
) -> Result<MoveFleetMemberOutcome, DomainError> {
    let fleet_id = input.fleet_id;
    let target_id = input.target_id;
    let new_role = input.new_role;

    let (resolved_wing, resolved_squad, before, after, is_only_member, target_sys_id) =
        ctx.db.try_with_mut_snapshot(|snap| {
            let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                return Err(DomainError::not_found(
                    "FleetNotFound",
                    format!("Fleet {fleet_id} not found"),
                ));
            };
            if !is_fleet_member(fleet, target_id) {
                return Err(DomainError::not_found(
                    "MemberNotFound",
                    format!("Character {target_id} is not in fleet {fleet_id}"),
                ));
            }
            let (resolved_wing, resolved_squad) = if input.strict_esi_wing_squad {
                validate_wing_and_squad(
                    fleet,
                    fleet_id,
                    new_role,
                    input.wing_id,
                    input.squad_id,
                    true,
                )?
            } else {
                (
                    Some(input.wing_id.unwrap_or(1)),
                    Some(input.squad_id.unwrap_or(1)),
                )
            };
            if let Some(caller_id) = input.caller_char_id {
                let self_free_move =
                    target_id == caller_id && fleet.free_move && new_role == FleetRole::SquadMember;
                if !self_free_move {
                    if !fleet.member_details.contains_key(&caller_id) && fleet.boss_id != caller_id
                    {
                        return Err(DomainError::forbidden(
                            "NotInFleet",
                            "Caller is not in fleet",
                        ));
                    }
                    if !can_act_on_member(
                        fleet,
                        caller_id,
                        target_id,
                        Some((new_role, resolved_wing, resolved_squad)),
                    ) {
                        return Err(DomainError::forbidden(
                            "FleetPermissionDenied",
                            "Insufficient fleet hierarchy authority",
                        ));
                    }
                }
            }

            if new_role == FleetRole::FleetCommander
                && fleet
                    .member_details
                    .values()
                    .any(|m| m.character_id != target_id && m.role == FleetRole::FleetCommander)
            {
                return Err(DomainError::conflict(
                    "PositionOccupied",
                    "Fleet Commander slot is already occupied",
                ));
            }
            if new_role == FleetRole::WingCommander
                && resolved_wing.is_some()
                && fleet.member_details.values().any(|m| {
                    m.character_id != target_id
                        && m.wing_id == resolved_wing
                        && m.role == FleetRole::WingCommander
                })
            {
                return Err(DomainError::conflict(
                    "PositionOccupied",
                    "Wing Commander slot in target wing is already occupied",
                ));
            }
            if new_role == FleetRole::SquadCommander
                && resolved_squad.is_some()
                && fleet.member_details.values().any(|m| {
                    m.character_id != target_id
                        && m.squad_id == resolved_squad
                        && m.role == FleetRole::SquadCommander
                })
            {
                return Err(DomainError::conflict(
                    "PositionOccupied",
                    "Squad Commander slot in target squad is already occupied",
                ));
            }

            if input.enforce_session_change_timer
                && input.caller_char_id == Some(target_id)
                && crate::services::space_destiny::check_session_change_timer_active(
                    ctx,
                    target_id,
                    input.now_unix,
                    None,
                )
                .is_some()
            {
                return Err(DomainError::precondition_failed(
                    "SessionChangeTimerActive",
                    "Session change timer is active",
                ));
            }

            let existing = fleet.member_details.get(&target_id).cloned();
            if existing.is_none() && !input.allow_fallback_missing_wing_squad {
                return Err(DomainError::not_found(
                    "MemberNotFound",
                    format!("Character {target_id} is not in fleet {fleet_id}"),
                ));
            }
            let before = existing.unwrap_or_else(|| FleetMemberRecord {
                character_id: target_id,
                role: FleetRole::SquadMember,
                wing_id: None,
                squad_id: None,
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: role_name_str(FleetRole::SquadMember).to_string(),
                join_time_unix: 0,
            });
            let old_wing = before.wing_id;
            let old_squad = before.squad_id;
            let after = FleetMemberRecord {
                character_id: target_id,
                role: new_role,
                wing_id: resolved_wing,
                squad_id: resolved_squad,
                takes_fleet_warp: before.takes_fleet_warp,
                accepts_regroup: before.accepts_regroup,
                accepts_conduit_jumps: before.accepts_conduit_jumps,
                job: before.job,
                role_name: role_name_str(new_role).to_string(),
                join_time_unix: before.join_time_unix,
            };
            fleet.member_details.insert(target_id, after.clone());
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
                if let Some(nw) = resolved_wing {
                    if let Some(w) = fleet.wing_details.get_mut(&nw) {
                        w.commander_id = Some(target_id);
                    }
                }
            }
            if new_role == FleetRole::SquadCommander {
                if let Some(ns) = resolved_squad {
                    if let Some(s) = fleet.squad_details.get_mut(&ns) {
                        s.commander_id = Some(target_id);
                    }
                }
            }
            let target_sys = snap
                .characters
                .get(&target_id)
                .map(|c| c.solar_system_id)
                .unwrap_or(0);
            Ok((resolved_wing, resolved_squad, before, after, only, target_sys))
        })?;

    let self_moved = input.caller_char_id == Some(target_id);
    if self_moved && input.enforce_session_change_timer {
        crate::services::space_destiny::record_session_change_timer(
            ctx,
            target_id,
            input.now_unix,
            target_sys_id,
            0,
            "fleet_change",
        );
    }

    Ok(MoveFleetMemberOutcome {
        fleet_id,
        target_id,
        resolved_wing_id: resolved_wing,
        resolved_squad_id: resolved_squad,
        before,
        after,
        is_only_member: is_only_member,
        self_moved,
        now_unix: input.now_unix,
        target_solar_system_id: target_sys_id,
    })
}

/// Core protocol-agnostic fleet member removal:
/// - Cleans `fleet_extras` (disconnected, invite details/timestamps, watchlist, join requests)
/// - Cleans combat `authorized_shipping_storage`
/// - Removes from `members`, `member_details`, `invites`, wing/squad commanders, `fleetid:*` chat channel, and `character.fleet_id`
/// - Promotes successor boss or disbands fleet if empty
/// - Invalidates stale `fleetObjectHandler` bound objects via `"fleet_change"` session transition
pub fn remove_character_from_fleet_domain(
    ctx: &ServerContext,
    fleet_id: i64,
    char_id: i64,
) -> RemoveFleetMemberOutcome {
    {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        ex.disconnected_members.remove(&char_id);
        ex.invite_details.remove(&(fleet_id, char_id));
        ex.invite_timestamps.remove(&(fleet_id, char_id));
        ex.watchlist.remove(&char_id);
        for targets in ex.watchlist.values_mut() {
            targets.remove(&char_id);
        }
        if let Some(reqs) = ex.join_requests.get_mut(&fleet_id) {
            reqs.remove(&char_id);
        }
    }
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.authorized_shipping_storage
            .retain(|&(_, c)| c != char_id);
    });

    let disconnected: HashSet<i64> = ctx
        .fleet_extras
        .lock()
        .unwrap()
        .disconnected_members
        .keys()
        .copied()
        .collect();

    let (was_member, old_role_num, disbanded, hand_over, char_sys_id, char_dock_id) =
        ctx.db.with_mut_snapshot(|snap| {
            let mut was_in_fleet = false;
            let mut old_role_num = 4_i64;
            let mut should_disband = false;
            let mut hand_over: Option<(FleetMemberRecord, FleetMemberRecord, bool)> = None;

            if let Some(fleet) = snap.fleets.get_mut(&fleet_id) {
                fleet.invites.remove(&char_id);
                let old_len = fleet.members.len();
                fleet.members.retain(|&m| m != char_id);
                if fleet.members.len() != old_len {
                    was_in_fleet = true;
                }
                if let Some(removed) = fleet.member_details.remove(&char_id) {
                    was_in_fleet = true;
                    old_role_num = role_to_i64(removed.role);
                } else if fleet.boss_id == char_id {
                    old_role_num = 1;
                }
                for w in fleet.wing_details.values_mut() {
                    if w.commander_id == Some(char_id) {
                        w.commander_id = None;
                    }
                }
                for s in fleet.squad_details.values_mut() {
                    if s.commander_id == Some(char_id) {
                        s.commander_id = None;
                    }
                }
                if fleet.members.is_empty() && fleet.member_details.is_empty() {
                    should_disband = true;
                } else if fleet.boss_id == char_id {
                    let next_boss = fleet
                        .member_details
                        .values()
                        .min_by_key(|m| {
                            (
                                disconnected.contains(&m.character_id),
                                role_to_i64(m.role),
                                m.wing_id.unwrap_or(i64::MAX),
                                m.squad_id.unwrap_or(i64::MAX),
                                m.join_time_unix,
                                m.character_id,
                            )
                        })
                        .map(|m| m.character_id)
                        .or_else(|| {
                            fleet
                                .members
                                .iter()
                                .copied()
                                .find(|cid| !disconnected.contains(cid))
                        })
                        .or_else(|| fleet.members.first().copied())
                        .unwrap_or(0);
                    if next_boss != 0 {
                        fleet.boss_id = next_boss;
                        let only = fleet.members.len() == 1;
                        if let Some(m) = fleet.member_details.get_mut(&next_boss) {
                            let old = m.clone();
                            m.job |= FLEET_JOB_CREATOR;
                            hand_over = Some((old, m.clone(), only));
                        }
                    }
                }
            }
            let chan_key = format!("fleetid:{fleet_id}");
            if let Some(chan) = snap.chat_channels.get_mut(&chan_key) {
                chan.members.remove(&char_id);
            }
            let (sys_id, dock_id) = if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.fleet_id = None;
                (ch.solar_system_id, ch.structure_id.or(ch.station_id))
            } else {
                (0, None)
            };
            if should_disband {
                for ch in snap.characters.values_mut() {
                    if ch.fleet_id == Some(fleet_id) {
                        ch.fleet_id = None;
                    }
                }
                snap.fleets.remove(&fleet_id);
                snap.chat_channels.remove(&chan_key);
            }
            (
                was_in_fleet,
                old_role_num,
                should_disband,
                hand_over,
                sys_id,
                dock_id,
            )
        });

    if disbanded {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        ex.adverts.remove(&fleet_id);
        ex.fleet_names.remove(&fleet_id);
        ex.max_sizes.remove(&fleet_id);
        ex.auto_join_squads.remove(&fleet_id);
        ex.join_requests.remove(&fleet_id);
        ex.invite_details.retain(|&(fid, _), _| fid != fleet_id);
        ex.invite_timestamps.retain(|&(fid, _), _| fid != fleet_id);
    } else if let Some((_, new_boss, _)) = &hand_over {
        if let Some(ad) = ctx.fleet_extras.lock().unwrap().adverts.get_mut(&fleet_id) {
            if ad.leader_id == char_id {
                ad.leader_id = new_boss.character_id;
            }
        }
    }

    if was_member || disbanded {
        ctx.invalidate_bound_objects_for_session_transition(
            char_id,
            char_sys_id,
            char_dock_id,
            "fleet_change",
        );
    }

    RemoveFleetMemberOutcome {
        fleet_id,
        removed_char_id: char_id,
        was_member,
        old_role_num,
        disbanded,
        handover: hand_over,
    }
}

pub fn kick_or_remove_fleet_member(
    ctx: &ServerContext,
    fleet_id: i64,
    caller_char_id: Option<i64>,
    target_id: i64,
    forbid_boss_even_self: bool,
) -> Result<RemoveFleetMemberOutcome, DomainError> {
    ctx.db.with_snapshot(|snap| {
        let Some(fleet) = snap.fleets.get(&fleet_id) else {
            return Err(DomainError::not_found(
                "FleetNotFound",
                format!("Fleet {fleet_id} not found"),
            ));
        };
        if !is_fleet_member(fleet, target_id) {
            return Err(DomainError::not_found(
                "MemberNotFound",
                format!("Character {target_id} is not in fleet {fleet_id}"),
            ));
        }
        if let Some(caller_id) = caller_char_id {
            if !is_fleet_member(fleet, caller_id) {
                return Err(DomainError::forbidden(
                    "NotInFleet",
                    "Caller is not in fleet",
                ));
            }
        }
        if target_id == fleet.boss_id
            && (forbid_boss_even_self || caller_char_id.is_some_and(|cid| cid != target_id))
        {
            return Err(DomainError::forbidden(
                "CannotKickBoss",
                "Cannot kick or remove the fleet boss without transferring leadership",
            ));
        }
        if let Some(caller_id) = caller_char_id {
            if target_id != caller_id && !can_act_on_member(fleet, caller_id, target_id, None) {
                return Err(DomainError::forbidden(
                    "FleetPermissionDenied",
                    format!(
                        "Character {caller_id} lacks permission to kick member {target_id} from fleet {fleet_id}"
                    ),
                ));
            }
        }
        Ok(())
    })?;

    Ok(remove_character_from_fleet_domain(ctx, fleet_id, target_id))
}

pub fn disband_fleet(
    ctx: &ServerContext,
    fleet_id: i64,
    caller_id: i64,
) -> Result<DisbandFleetOutcome, DomainError> {
    let members_with_loc: Vec<(i64, u32, Option<i64>)> = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(fleet) = snap.fleets.get(&fleet_id) else {
            return Err(DomainError::not_found(
                "FleetNotFound",
                format!("Fleet {fleet_id} not found"),
            ));
        };
        if !is_fleet_member(fleet, caller_id) {
            return Err(DomainError::forbidden(
                "NotInFleet",
                "Caller is not in fleet",
            ));
        }
        if fleet.boss_id != caller_id {
            return Err(DomainError::forbidden(
                "FleetPermissionDenied",
                "Only the Fleet Boss can disband the fleet",
            ));
        }
        let mut member_ids = fleet.members.clone();
        for &cid in fleet.member_details.keys() {
            if !member_ids.contains(&cid) {
                member_ids.push(cid);
            }
        }
        let mut out = Vec::with_capacity(member_ids.len());
        for mid in member_ids {
            let (sys_id, dock_id) = if let Some(ch) = snap.characters.get_mut(&mid) {
                ch.fleet_id = None;
                (ch.solar_system_id, ch.structure_id.or(ch.station_id))
            } else {
                (0, None)
            };
            out.push((mid, sys_id, dock_id));
        }
        snap.fleets.remove(&fleet_id);
        snap.chat_channels.remove(&format!("fleetid:{fleet_id}"));
        Ok(out)
    })?;

    let member_ids: Vec<i64> = members_with_loc.iter().map(|(id, _, _)| *id).collect();
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
        ex.join_requests.remove(&fleet_id);
        for mid in &member_ids {
            ex.disconnected_members.remove(mid);
            ex.watchlist.remove(mid);
        }
        ex.voice_enabled.remove(&fleet_id);
        ex.loot_logging.remove(&fleet_id);
        ex.options.remove(&fleet_id);
        ex.wing_squad_hierarchy.remove(&fleet_id);
    }

    for (mid, sys_id, dock_id) in members_with_loc {
        ctx.invalidate_bound_objects_for_session_transition(mid, sys_id, dock_id, "fleet_change");
    }

    Ok(DisbandFleetOutcome {
        fleet_id,
        caller_id,
        former_member_ids: member_ids,
    })
}

pub fn list_fleet_wings(
    ctx: &ServerContext,
    fleet_id: i64,
    caller_char_id: Option<i64>,
) -> Result<Vec<WingSummary>, DomainError> {
    ctx.db.with_snapshot(|snap| {
        let Some(f) = snap.fleets.get(&fleet_id) else {
            return Err(DomainError::not_found(
                "FleetNotFound",
                format!("Fleet {fleet_id} not found"),
            ));
        };
        if let Some(cid) = caller_char_id {
            if !is_fleet_member(f, cid) {
                return Err(DomainError::forbidden(
                    "NotInFleet",
                    format!("Character {cid} is not a member of fleet {fleet_id}"),
                ));
            }
        }
        let mut wings = Vec::new();
        let mut wing_ids = f.wings.clone();
        for &wid in f.wing_details.keys() {
            if !wing_ids.contains(&wid) {
                wing_ids.push(wid);
            }
        }
        wing_ids.sort_unstable();
        for wid in wing_ids {
            let w_name = f
                .wing_details
                .get(&wid)
                .map(|w| w.name.clone())
                .unwrap_or_else(|| format!("Wing {wid}"));
            let mut squad_ids: Vec<i64> = f
                .wing_details
                .get(&wid)
                .map(|w| w.squad_ids.clone())
                .unwrap_or_default();
            for (sid, sq) in &f.squad_details {
                if sq.wing_id == wid && !squad_ids.contains(sid) {
                    squad_ids.push(*sid);
                }
            }
            if squad_ids.is_empty() && f.squad_details.is_empty() {
                squad_ids.extend(f.squads.iter().copied());
            }
            squad_ids.sort_unstable();
            let squads = squad_ids
                .into_iter()
                .map(|sid| {
                    let s_name = f
                        .squad_details
                        .get(&sid)
                        .map(|s| s.name.clone())
                        .unwrap_or_else(|| format!("Squad {sid}"));
                    SquadSummary {
                        squad_id: sid,
                        name: s_name,
                    }
                })
                .collect();
            wings.push(WingSummary {
                wing_id: wid,
                name: w_name,
                squads,
            });
        }
        Ok(wings)
    })
}

pub fn create_fleet_wing(
    ctx: &ServerContext,
    fleet_id: i64,
    caller_char_id: Option<i64>,
    name_override: Option<&str>,
    enforce_max_wings: bool,
    min_entity_id: i64,
    use_fleet_local_id: bool,
) -> Result<WingCreated, DomainError> {
    ctx.db.try_with_mut_snapshot(|snap| {
        let Some(f) = snap.fleets.get(&fleet_id) else {
            return Err(DomainError::not_found(
                "FleetNotFound",
                format!("Fleet {fleet_id} not found"),
            ));
        };
        if let Some(cid) = caller_char_id {
            if !is_fleet_member(f, cid) {
                return Err(DomainError::forbidden(
                    "NotInFleet",
                    "Caller is not in fleet",
                ));
            }
            if !is_fleet_boss_or_fc(f, cid) {
                return Err(DomainError::forbidden(
                    "FleetPermissionDenied",
                    format!(
                        "Character {cid} lacks fleet commander/boss permissions for fleet {fleet_id}"
                    ),
                ));
            }
        }
        if enforce_max_wings && f.wings.len() >= 5 {
            return Err(DomainError::precondition_failed(
                "MaxWingsReached",
                "Fleet already has the maximum of 5 wings",
            ));
        }
        let wing_id = if use_fleet_local_id {
            (f.wings.iter().max().copied().unwrap_or(0) + 1).max(min_entity_id)
        } else {
            snap.next_entity_id += 1;
            let id = snap.next_entity_id.max(min_entity_id);
            snap.next_entity_id = id;
            id
        };
        let f = snap.fleets.get_mut(&fleet_id).unwrap();
        let name = name_override
            .map(str::to_string)
            .unwrap_or_else(|| format!("Wing {}", f.wings.len() + 1));
        f.wings.push(wing_id);
        f.wing_details.insert(
            wing_id,
            FleetWingRecord {
                wing_id,
                name: name.clone(),
                commander_id: None,
                squad_ids: Vec::new(),
            },
        );
        Ok(WingCreated {
            fleet_id,
            wing_id,
            name,
        })
    })
}

pub fn rename_fleet_wing(
    ctx: &ServerContext,
    fleet_id: i64,
    caller_char_id: Option<i64>,
    wing_id: i64,
    new_name: &str,
    validate_name_len: bool,
) -> Result<String, DomainError> {
    let target_name = if validate_name_len {
        let trimmed = new_name.trim();
        if trimmed.is_empty() || trimmed.len() > 10 {
            return Err(DomainError::invalid_input(
                "InvalidArgument",
                "Wing 'name' must be 1..=10 characters",
            ));
        }
        trimmed.to_string()
    } else {
        new_name.to_string()
    };

    ctx.db.try_with_mut_snapshot(|snap| {
        let Some(f) = snap.fleets.get_mut(&fleet_id) else {
            return Err(DomainError::not_found(
                "FleetNotFound",
                format!("Fleet {fleet_id} not found"),
            ));
        };
        if let Some(cid) = caller_char_id {
            if !is_fleet_member(f, cid) {
                return Err(DomainError::forbidden(
                    "NotInFleet",
                    "Caller is not in fleet",
                ));
            }
            if !can_manage_wing(f, cid, wing_id) {
                return Err(DomainError::forbidden(
                    "FleetPermissionDenied",
                    format!(
                        "Character {cid} lacks wing/fleet commander permissions for wing {wing_id}"
                    ),
                ));
            }
        }
        if !f.wings.contains(&wing_id) && !f.wing_details.contains_key(&wing_id) {
            return Err(DomainError::not_found(
                "WingNotFound",
                format!("Wing {wing_id} not found in fleet {fleet_id}"),
            ));
        }
        let entry = f.wing_details.entry(wing_id).or_insert_with(|| FleetWingRecord {
            wing_id,
            name: target_name.clone(),
            commander_id: None,
            squad_ids: Vec::new(),
        });
        entry.name = target_name.clone();
        Ok(target_name)
    })
}

pub fn delete_fleet_wing(
    ctx: &ServerContext,
    fleet_id: i64,
    caller_char_id: Option<i64>,
    wing_id: i64,
    forbid_non_empty: bool,
) -> Result<(), DomainError> {
    ctx.db.try_with_mut_snapshot(|snap| {
        let Some(f) = snap.fleets.get_mut(&fleet_id) else {
            return Err(DomainError::not_found(
                "FleetNotFound",
                format!("Fleet {fleet_id} not found"),
            ));
        };
        if let Some(cid) = caller_char_id {
            if !is_fleet_member(f, cid) {
                return Err(DomainError::forbidden(
                    "NotInFleet",
                    "Caller is not in fleet",
                ));
            }
            if !is_fleet_boss_or_fc(f, cid) {
                return Err(DomainError::forbidden(
                    "FleetPermissionDenied",
                    format!(
                        "Character {cid} lacks fleet commander/boss permissions for fleet {fleet_id}"
                    ),
                ));
            }
        }
        if forbid_non_empty && f.member_details.values().any(|m| m.wing_id == Some(wing_id)) {
            return Err(DomainError::conflict(
                "WingNotEmpty",
                "Cannot delete a wing that still contains members",
            ));
        }
        if !f.wings.contains(&wing_id) && !f.wing_details.contains_key(&wing_id) {
            return Err(DomainError::not_found(
                "WingNotFound",
                format!("Wing {wing_id} not found in fleet {fleet_id}"),
            ));
        }
        f.wings.retain(|&w| w != wing_id);
        if let Some(removed_wing) = f.wing_details.remove(&wing_id) {
            for sid in removed_wing.squad_ids {
                f.squads.retain(|&s| s != sid);
                f.squad_details.remove(&sid);
            }
        }
        f.squad_details.retain(|sid, sq| {
            if sq.wing_id == wing_id {
                f.squads.retain(|&s| s != *sid);
                false
            } else {
                true
            }
        });
        Ok(())
    })
}

pub fn create_fleet_squad(
    ctx: &ServerContext,
    fleet_id: i64,
    caller_char_id: Option<i64>,
    wing_id: i64,
    name_override: Option<&str>,
    enforce_max_squads: bool,
    min_entity_id: i64,
    use_fleet_local_id: bool,
) -> Result<SquadCreated, DomainError> {
    ctx.db.try_with_mut_snapshot(|snap| {
        let Some(f) = snap.fleets.get(&fleet_id) else {
            return Err(DomainError::not_found(
                "FleetNotFound",
                format!("Fleet {fleet_id} not found"),
            ));
        };
        if !f.wings.contains(&wing_id) && !f.wing_details.contains_key(&wing_id) {
            return Err(DomainError::not_found(
                "WingNotFound",
                format!("Wing {wing_id} not found in fleet {fleet_id}"),
            ));
        }
        if let Some(cid) = caller_char_id {
            if !is_fleet_member(f, cid) {
                return Err(DomainError::forbidden(
                    "NotInFleet",
                    "Caller is not in fleet",
                ));
            }
            if !can_manage_wing(f, cid, wing_id) {
                return Err(DomainError::forbidden(
                    "FleetPermissionDenied",
                    format!(
                        "Character {cid} lacks wing/fleet commander permissions for wing {wing_id}"
                    ),
                ));
            }
        }
        if enforce_max_squads
            && f.wing_details
                .get(&wing_id)
                .is_some_and(|w| w.squad_ids.len() >= 5)
        {
            return Err(DomainError::precondition_failed(
                "MaxSquadsReached",
                "Wing already has the maximum of 5 squads",
            ));
        }
        let squad_id = if use_fleet_local_id {
            (f.squads.iter().max().copied().unwrap_or(0) + 1).max(min_entity_id)
        } else {
            snap.next_entity_id += 1;
            let id = snap.next_entity_id.max(min_entity_id);
            snap.next_entity_id = id;
            id
        };
        let f = snap.fleets.get_mut(&fleet_id).unwrap();
        let name = name_override
            .map(str::to_string)
            .unwrap_or_else(|| format!("Squad {}", f.squads.len() + 1));
        f.squads.push(squad_id);
        f.squad_details.insert(
            squad_id,
            FleetSquadRecord {
                squad_id,
                wing_id,
                name: name.clone(),
                commander_id: None,
            },
        );
        let wing = f.wing_details.entry(wing_id).or_insert_with(|| FleetWingRecord {
            wing_id,
            name: format!("Wing {wing_id}"),
            commander_id: None,
            squad_ids: Vec::new(),
        });
        if !wing.squad_ids.contains(&squad_id) {
            wing.squad_ids.push(squad_id);
        }
        Ok(SquadCreated {
            fleet_id,
            wing_id,
            squad_id,
            name,
        })
    })
}

pub fn rename_fleet_squad(
    ctx: &ServerContext,
    fleet_id: i64,
    caller_char_id: Option<i64>,
    squad_id: i64,
    new_name: &str,
    validate_name_len: bool,
) -> Result<String, DomainError> {
    let target_name = if validate_name_len {
        let trimmed = new_name.trim();
        if trimmed.is_empty() || trimmed.len() > 10 {
            return Err(DomainError::invalid_input(
                "InvalidArgument",
                "Squad 'name' must be 1..=10 characters",
            ));
        }
        trimmed.to_string()
    } else {
        new_name.to_string()
    };

    ctx.db.try_with_mut_snapshot(|snap| {
        let Some(f) = snap.fleets.get_mut(&fleet_id) else {
            return Err(DomainError::not_found(
                "FleetNotFound",
                format!("Fleet {fleet_id} not found"),
            ));
        };
        if !f.squads.contains(&squad_id) && !f.squad_details.contains_key(&squad_id) {
            return Err(DomainError::not_found(
                "SquadNotFound",
                format!("Squad {squad_id} not found in fleet {fleet_id}"),
            ));
        }
        if let Some(cid) = caller_char_id {
            if !is_fleet_member(f, cid) {
                return Err(DomainError::forbidden(
                    "NotInFleet",
                    "Caller is not in fleet",
                ));
            }
            if !can_manage_squad(f, cid, squad_id) {
                return Err(DomainError::forbidden(
                    "FleetPermissionDenied",
                    format!(
                        "Character {cid} lacks squad/wing/fleet commander permissions for squad {squad_id}"
                    ),
                ));
            }
        }
        let default_wing = f.wings.first().copied().unwrap_or(1);
        let sq = f.squad_details.entry(squad_id).or_insert_with(|| FleetSquadRecord {
            squad_id,
            wing_id: default_wing,
            name: target_name.clone(),
            commander_id: None,
        });
        sq.name = target_name.clone();
        Ok(target_name)
    })
}

pub fn delete_fleet_squad(
    ctx: &ServerContext,
    fleet_id: i64,
    caller_char_id: Option<i64>,
    squad_id: i64,
    forbid_non_empty: bool,
) -> Result<(), DomainError> {
    ctx.db.try_with_mut_snapshot(|snap| {
        let Some(f) = snap.fleets.get_mut(&fleet_id) else {
            return Err(DomainError::not_found(
                "FleetNotFound",
                format!("Fleet {fleet_id} not found"),
            ));
        };
        if !f.squads.contains(&squad_id) && !f.squad_details.contains_key(&squad_id) {
            return Err(DomainError::not_found(
                "SquadNotFound",
                format!("Squad {squad_id} not found in fleet {fleet_id}"),
            ));
        }
        if let Some(cid) = caller_char_id {
            if !is_fleet_member(f, cid) {
                return Err(DomainError::forbidden(
                    "NotInFleet",
                    "Caller is not in fleet",
                ));
            }
            if !can_manage_squad(f, cid, squad_id) {
                return Err(DomainError::forbidden(
                    "FleetPermissionDenied",
                    format!(
                        "Character {cid} lacks squad/wing/fleet commander permissions for squad {squad_id}"
                    ),
                ));
            }
        }
        if forbid_non_empty && f.member_details.values().any(|m| m.squad_id == Some(squad_id)) {
            return Err(DomainError::conflict(
                "SquadNotEmpty",
                "Cannot delete a squad that still contains members",
            ));
        }
        f.squads.retain(|&s| s != squad_id);
        f.squad_details.remove(&squad_id);
        for wing in f.wing_details.values_mut() {
            wing.squad_ids.retain(|&s| s != squad_id);
        }
        Ok(())
    })
}
