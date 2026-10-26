//! `quasar_goal_tale_notices` — wire builders for the V24.01 public-gateway notices the client
//! subscribes to for corporation projects (`corporation/client/goals/goalMessenger.pyj`) and
//! tales (`taleclient/listener/notice_listener.pyj`).
//!
//! Field numbers follow the client's generated descriptors:
//! - `eve_public.goal.api.{Created,Deleted,Closed,Completed,Progressed,NameChanged,DescriptionChanged}Notice`
//! - `eve_public.goal.contribution.api.ContributedNotice`
//! - `eve_public.tale.api.{Started,Stopped}Notice`
//!
//! The builders are pure; callers emit them only from real state transitions (goal create /
//! close / delete / progress / rename handlers, gameplay contributions, tale lifecycle).

#![forbid(unsafe_code)]

use crate::quasar_gateway::NoticeScope;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};

/// Prefix of the 16-byte goal UUID used for goals backed by the corporation project ledger.
pub const CORP_GOAL_UUID_PREFIX: &[u8; 8] = b"CORPGOAL";

/// `eve_public.goal.State` enum values (goal_pb2).
pub const GOAL_STATE_ACTIVE: u64 = 1;
pub const GOAL_STATE_CLOSED: u64 = 2;
pub const GOAL_STATE_COMPLETED: u64 = 3;

fn any(type_name: &str, value: Vec<u8>) -> GoogleAny {
    GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }
}

/// Encodes `eve_public.goal.Identifier { uuid = 1 }` as a nested message at `field_num`.
fn put_goal_id(field_num: u32, uuid: &[u8; 16], out: &mut Vec<u8>) {
    let mut id = Vec::new();
    encode_bytes_field(1, uuid, &mut id);
    encode_bytes_field(field_num, &id, out);
}

/// Encodes an `Identifier { sequential = 1 }` (character, tale, spawntemplate, solarsystem).
fn put_seq_id(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, id, &mut b);
    encode_bytes_field(field_num, &b, out);
}

/// Encodes `google.protobuf.Timestamp { seconds = 1 }`.
fn put_timestamp(field_num: u32, unix: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, unix, &mut b);
    encode_bytes_field(field_num, &b, out);
}

/// Resolves the notice audience for a goal: corporation projects are broadcast to every member
/// of the owning corporation (the client's project UI is corporation-wide); personal goals go to
/// their creator only.
pub fn goal_scope(corp_id: Option<u32>, creator_id: u32) -> NoticeScope {
    match corp_id {
        Some(c) if c > 0 => NoticeScope::Corporation(c),
        _ => NoticeScope::Character(creator_id),
    }
}

/// Snapshot of the goal attributes carried by `CreatedNotice.goal` (`eve_public.goal.Attributes`).
pub struct GoalAttributesView<'a> {
    pub created_unix: u64,
    pub creator_id: u32,
    pub corp_id: Option<u32>,
    pub name: &'a str,
    pub description: &'a str,
    pub desired: u64,
    pub current: u64,
}

/// `eve_public.goal.api.CreatedNotice { id = 1, goal = 2 }`.
pub fn goal_created(uuid: &[u8; 16], attrs: &GoalAttributesView<'_>) -> GoogleAny {
    let mut a = Vec::new();
    put_timestamp(1, attrs.created_unix, &mut a);
    encode_string_field(2, attrs.name, &mut a); // user_input_name
    encode_string_field(3, attrs.description, &mut a); // user_input_description
    put_seq_id(4, attrs.creator_id as u64, &mut a); // creator
    let mut progress = Vec::new();
    encode_varint_field(1, attrs.desired, &mut progress);
    encode_varint_field(2, attrs.current, &mut progress);
    encode_bytes_field(5, &progress, &mut a);
    let state = if attrs.current >= attrs.desired { GOAL_STATE_COMPLETED } else { GOAL_STATE_ACTIVE };
    encode_varint_field(6, state, &mut a);
    if let Some(corp) = attrs.corp_id.filter(|c| *c > 0) {
        // Attributes.organization = Organization { corporation = 1 }
        let mut org = Vec::new();
        put_seq_id(1, corp as u64, &mut org);
        encode_bytes_field(9, &org, &mut a);
    }
    encode_varint_field(10, 1, &mut a); // finish_time.not_finished = true

    let mut out = Vec::new();
    put_goal_id(1, uuid, &mut out);
    encode_bytes_field(2, &a, &mut out);
    any("eve_public.goal.api.CreatedNotice", out)
}

/// `eve_public.goal.api.DeletedNotice { goal = 1 }`.
pub fn goal_deleted(uuid: &[u8; 16]) -> GoogleAny {
    let mut out = Vec::new();
    put_goal_id(1, uuid, &mut out);
    any("eve_public.goal.api.DeletedNotice", out)
}

/// `eve_public.goal.api.ClosedNotice { goal = 1, closer = 2 }`.
pub fn goal_closed(uuid: &[u8; 16], closer_id: u32) -> GoogleAny {
    let mut out = Vec::new();
    put_goal_id(1, uuid, &mut out);
    put_seq_id(2, closer_id as u64, &mut out);
    any("eve_public.goal.api.ClosedNotice", out)
}

/// `eve_public.goal.api.CompletedNotice { goal = 1 }`.
pub fn goal_completed(uuid: &[u8; 16]) -> GoogleAny {
    let mut out = Vec::new();
    put_goal_id(1, uuid, &mut out);
    any("eve_public.goal.api.CompletedNotice", out)
}

/// `eve_public.goal.api.ProgressedNotice { goal = 1, current_progress = 2 }`.
pub fn goal_progressed(uuid: &[u8; 16], current: u64) -> GoogleAny {
    let mut out = Vec::new();
    put_goal_id(1, uuid, &mut out);
    encode_varint_field(2, current, &mut out);
    any("eve_public.goal.api.ProgressedNotice", out)
}

/// `eve_public.goal.api.NameChangedNotice { goal = 1, admin_designated_new_name = 2 }`.
pub fn goal_name_changed(uuid: &[u8; 16], new_name: &str) -> GoogleAny {
    let mut out = Vec::new();
    put_goal_id(1, uuid, &mut out);
    encode_string_field(2, new_name, &mut out);
    any("eve_public.goal.api.NameChangedNotice", out)
}

/// `eve_public.goal.api.DescriptionChangedNotice { goal = 1, admin_designated_new_description = 2 }`.
pub fn goal_description_changed(uuid: &[u8; 16], new_description: &str) -> GoogleAny {
    let mut out = Vec::new();
    put_goal_id(1, uuid, &mut out);
    encode_string_field(2, new_description, &mut out);
    any("eve_public.goal.api.DescriptionChangedNotice", out)
}

/// `eve_public.goal.contribution.api.ContributedNotice
///  { timestamp = 1, contributor = 2, goal = 3, previous_progress = 4, current_progress = 5 }`.
pub fn goal_contributed(uuid: &[u8; 16], contributor_id: u32, previous: u64, current: u64, now_unix: u64) -> GoogleAny {
    let mut out = Vec::new();
    put_timestamp(1, now_unix, &mut out);
    put_seq_id(2, contributor_id as u64, &mut out);
    put_goal_id(3, uuid, &mut out);
    encode_varint_field(4, previous, &mut out);
    encode_varint_field(5, current, &mut out);
    any("eve_public.goal.contribution.api.ContributedNotice", out)
}

/// `eve_public.tale.api.StartedNotice` / `StoppedNotice`
/// `{ tale = 1, spawntemplate = 2, solar_system = 3 }`.
pub fn tale_lifecycle(started: bool, tale_id: u64, template_id: u64, solar_system_id: u64) -> GoogleAny {
    let mut out = Vec::new();
    put_seq_id(1, tale_id, &mut out);
    put_seq_id(2, template_id, &mut out);
    put_seq_id(3, solar_system_id, &mut out);
    let name = if started { "eve_public.tale.api.StartedNotice" } else { "eve_public.tale.api.StoppedNotice" };
    any(name, out)
}

/// Audience for tale lifecycle notices. The client's tale service keeps a universe-wide
/// "active tales by template" cache (`get_global_world_event_tales`) that it invalidates on
/// either notice, so tale lifecycle is broadcast to every subscribed session.
pub fn tale_scope() -> NoticeScope {
    NoticeScope::Broadcast
}
