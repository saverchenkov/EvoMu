//! Wire encoders and field extractors for `quasar_career_goals`.

#![forbid(unsafe_code)]

use super::quasar_career_goals::{
    career_milestone_uuid, CareerGoalDef, CareerMilestoneDef, CharacterCareerGoal,
    CAREER_PATH_MESSAGE_IDS,
};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, encode_zigzag64,
    DynamicQuasarMessage, DynamicValue, GoogleAny, GoogleTimestamp, QuasarMessage, WireMessage,
};

pub fn encode_career_goal_identifier(uuid: &[u8; 16], out: &mut Vec<u8>) {
    encode_bytes_field(1, uuid, out);
}

fn encode_ui_annotation(key: &str, value: &str) -> Vec<u8> {
    let mut ann = Vec::new();
    encode_string_field(1, key, &mut ann);
    encode_string_field(2, value, &mut ann);
    ann
}

fn encode_career_goal_rewards(def: &CareerGoalDef, field_num: u32, out: &mut Vec<u8>) {
    if def.isk_reward > 0 {
        let mut currency = Vec::new();
        encode_varint_field(1, encode_zigzag64(def.isk_reward), &mut currency);
        let mut rtype = Vec::new();
        encode_bytes_field(2, &currency, &mut rtype); // RewardType.isk = 2
        let mut rew = Vec::new();
        encode_bytes_field(1, &rtype, &mut rew);
        encode_varint_field(2, 1, &mut rew); // REWARD_LABEL_ALPHA = 1
        encode_bytes_field(field_num, &rew, out);
    }
    if def.sp_reward > 0.0 {
        let mut rtype = Vec::new();
        encode_varint_field(3, def.sp_reward as u64, &mut rtype); // RewardType.skill_points = 3
        let mut rew = Vec::new();
        encode_bytes_field(1, &rtype, &mut rew);
        encode_varint_field(2, 1, &mut rew); // REWARD_LABEL_ALPHA = 1
        encode_bytes_field(field_num, &rew, out);
    }
}

/// Wire encoder for `eve_public.career.goal.api.GetAllResponse.Goal`.
pub fn encode_career_goal_wire(g: &CharacterCareerGoal, out: &mut Vec<u8>) {
    let mut id_bytes = Vec::new();
    encode_career_goal_identifier(&g.uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, out);

    encode_varint_field(2, g.progress, out);
    encode_varint_field(3, if g.completed { 0 } else { 1 }, out);

    if let Some(ts) = g.completed_time {
        let ts_proto = GoogleTimestamp {
            seconds: ts as i64,
            nanos: 0,
        };
        encode_bytes_field(4, &ts_proto.encode_proto(), out);
    }

    let mut label_state = Vec::new();
    encode_varint_field(1, 1, &mut label_state); // REWARD_LABEL_ALPHA
    if g.claimed {
        let ts_proto = GoogleTimestamp {
            seconds: g.completed_time.unwrap_or(0) as i64,
            nanos: 0,
        };
        encode_bytes_field(2, &ts_proto.encode_proto(), &mut label_state);
    } else {
        encode_varint_field(3, 1, &mut label_state); // not_claimed = true
    }
    encode_bytes_field(5, &label_state, out);
}

fn encode_career_goal_ui_annotations(def: &CareerGoalDef, field_num: u32, attr_bytes: &mut Vec<u8>) {
    encode_bytes_field(
        field_num,
        &encode_ui_annotation("group", def.effective_group()),
        attr_bytes,
    );
    encode_bytes_field(
        field_num,
        &encode_ui_annotation("subtype", "CAREER_PATH_GOAL"),
        attr_bytes,
    );
    encode_bytes_field(
        field_num,
        &encode_ui_annotation("name", &def.name),
        attr_bytes,
    );
    encode_bytes_field(
        field_num,
        &encode_ui_annotation("description", &def.description),
        attr_bytes,
    );
    encode_bytes_field(
        field_num,
        &encode_ui_annotation("duration", &def.effective_duration().to_string()),
        attr_bytes,
    );
    encode_bytes_field(
        field_num,
        &encode_ui_annotation("auraTextID", &def.effective_aura_text_id().to_string()),
        attr_bytes,
    );
    encode_bytes_field(
        field_num,
        &encode_ui_annotation("order", &def.effective_order().to_string()),
        attr_bytes,
    );
    if let Some(link_id) = def.agency_link_text_id.filter(|&id| id > 0) {
        encode_bytes_field(
            field_num,
            &encode_ui_annotation("agencyLinkTextID", &link_id.to_string()),
            attr_bytes,
        );
        let screen = def
            .agency_screen
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("HOMEPAGE");
        encode_bytes_field(
            field_num,
            &encode_ui_annotation("agencyScreen", screen),
            attr_bytes,
        );
    }
}

/// Returns authentic V24.01 `(name_msg_id, description_msg_id)` for AIR Career Program milestones (#638).
fn career_milestone_message_ids(ms: &CareerMilestoneDef) -> (u32, u32) {
    match ms.milestone_id {
        1 => (602_236, 610_011),   // Explorer Graduation
        2 => (602_238, 610_012),   // Industrialist Graduation
        3 => (602_240, 610_013),   // Enforcer Graduation
        4 => (602_242, 610_014),   // Soldier of Fortune Graduation
        101 => (599_620, 599_619), // Overall 750 CP
        102 => (602_248, 602_246), // Overall 1500 CP
        103 => (602_250, 602_251), // Overall 2250 CP
        104 => (602_252, 602_253), // Overall 3000 CP
        _ => {
            let fallback = ms
                .career_id
                .and_then(|cid| {
                    CAREER_PATH_MESSAGE_IDS
                        .iter()
                        .find(|(id, _, _)| *id == cid)
                        .map(|(_, n, d)| (*n, *d))
                })
                .unwrap_or((599_620, 599_619));
            fallback
        }
    }
}

/// Wire encoder for `eve_public.career.goal.api.GetDefinitionsResponse.Goal`.
pub fn encode_career_goal_definition_wire(def: &CareerGoalDef, out: &mut Vec<u8>) {
    let mut id_bytes = Vec::new();
    encode_career_goal_identifier(&def.uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, out);

    let mut attr_bytes = Vec::new();
    let mut name_id = Vec::new();
    encode_varint_field(1, def.effective_name_message_id() as u64, &mut name_id);
    encode_bytes_field(1, &name_id, &mut attr_bytes);

    let mut desc_id = Vec::new();
    encode_varint_field(1, def.effective_description_message_id() as u64, &mut desc_id);
    encode_bytes_field(2, &desc_id, &mut attr_bytes);

    encode_varint_field(3, 0, &mut attr_bytes); // progress
    encode_varint_field(4, def.target.max(1), &mut attr_bytes); // target
    encode_career_goal_ui_annotations(def, 6, &mut attr_bytes);
    encode_varint_field(7, def.effective_threat() as u64, &mut attr_bytes); // threat
    encode_varint_field(8, 0, &mut attr_bytes); // completed = false
    encode_varint_field(9, def.career_id.clamp(1, 4) as u64, &mut attr_bytes); // career enum
    encode_varint_field(10, def.career_points as u64, &mut attr_bytes); // career_points
    encode_varint_field(11, 0, &mut attr_bytes); // claimed = false
    encode_career_goal_rewards(def, 12, &mut attr_bytes);

    encode_bytes_field(2, &attr_bytes, out);
}

pub fn encode_career_milestone_definition_wire(ms: &CareerMilestoneDef, out: &mut Vec<u8>) {
    let uuid = career_milestone_uuid(ms.milestone_id);
    let mut id_bytes = Vec::new();
    encode_career_goal_identifier(&uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, out);

    let mut attr_bytes = Vec::new();
    let (name_msg_id, desc_msg_id) = career_milestone_message_ids(ms);

    let mut name_id = Vec::new();
    encode_varint_field(1, name_msg_id as u64, &mut name_id);
    encode_bytes_field(1, &name_id, &mut attr_bytes);

    let mut desc_id = Vec::new();
    encode_varint_field(1, desc_msg_id as u64, &mut desc_id);
    encode_bytes_field(2, &desc_id, &mut attr_bytes);

    encode_varint_field(3, 0, &mut attr_bytes);
    encode_varint_field(4, ms.required_points.max(1) as u64, &mut attr_bytes);
    let (group, subtype) = if ms.career_id.is_some() {
        ("Career Graduation", "CAREER_PATH_GOAL")
    } else {
        ("Overall", "OVERALL_GOAL")
    };
    encode_bytes_field(6, &encode_ui_annotation("group", group), &mut attr_bytes);
    encode_bytes_field(6, &encode_ui_annotation("subtype", subtype), &mut attr_bytes);
    encode_bytes_field(6, &encode_ui_annotation("name", &ms.description), &mut attr_bytes);
    encode_bytes_field(6, &encode_ui_annotation("description", &ms.description), &mut attr_bytes);
    encode_bytes_field(6, &encode_ui_annotation("duration", "0"), &mut attr_bytes);
    encode_bytes_field(6, &encode_ui_annotation("auraTextID", &desc_msg_id.to_string()), &mut attr_bytes);
    encode_bytes_field(6, &encode_ui_annotation("order", &ms.milestone_id.to_string()), &mut attr_bytes);
    encode_varint_field(7, 1, &mut attr_bytes);
    encode_varint_field(8, 0, &mut attr_bytes);
    encode_varint_field(
        9,
        ms.career_id.map(|c| c.clamp(1, 4) as u64).unwrap_or(0),
        &mut attr_bytes,
    );
    encode_varint_field(10, 0, &mut attr_bytes);
    encode_varint_field(11, 0, &mut attr_bytes);
    if ms.sp_reward > 0.0 {
        let mut rtype = Vec::new();
        encode_varint_field(3, ms.sp_reward as u64, &mut rtype);
        let mut rew = Vec::new();
        encode_bytes_field(1, &rtype, &mut rew);
        encode_varint_field(2, 1, &mut rew);
        encode_bytes_field(12, &rew, &mut attr_bytes);
    }

    encode_bytes_field(2, &attr_bytes, out);
}

pub fn encode_internal_career_milestone_definition_wire(ms: &CareerMilestoneDef, out: &mut Vec<u8>) {
    let uuid = career_milestone_uuid(ms.milestone_id);
    let mut id_bytes = Vec::new();
    encode_career_goal_identifier(&uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, out);

    let mut attr_bytes = Vec::new();
    let (name_msg_id, desc_msg_id) = career_milestone_message_ids(ms);

    let mut name_id = Vec::new();
    encode_varint_field(1, name_msg_id as u64, &mut name_id);
    encode_bytes_field(1, &name_id, &mut attr_bytes);

    let mut desc_id = Vec::new();
    encode_varint_field(1, desc_msg_id as u64, &mut desc_id);
    encode_bytes_field(2, &desc_id, &mut attr_bytes);

    encode_varint_field(3, ms.required_points.max(1) as u64, &mut attr_bytes);
    let (group, subtype) = if ms.career_id.is_some() {
        ("Career Graduation", "CAREER_PATH_GOAL")
    } else {
        ("Overall", "OVERALL_GOAL")
    };
    encode_bytes_field(5, &encode_ui_annotation("group", group), &mut attr_bytes);
    encode_bytes_field(5, &encode_ui_annotation("subtype", subtype), &mut attr_bytes);
    encode_bytes_field(5, &encode_ui_annotation("name", &ms.description), &mut attr_bytes);
    encode_bytes_field(5, &encode_ui_annotation("duration", "0"), &mut attr_bytes);
    encode_bytes_field(5, &encode_ui_annotation("auraTextID", &desc_msg_id.to_string()), &mut attr_bytes);
    encode_bytes_field(5, &encode_ui_annotation("order", &ms.milestone_id.to_string()), &mut attr_bytes);
    encode_varint_field(6, 1, &mut attr_bytes);
    encode_varint_field(
        7,
        ms.career_id.map(|c| c.clamp(1, 4) as u64).unwrap_or(0),
        &mut attr_bytes,
    );
    encode_varint_field(8, 0, &mut attr_bytes);
    if ms.sp_reward > 0.0 {
        let mut rtype = Vec::new();
        encode_varint_field(3, ms.sp_reward as u64, &mut rtype);
        let mut rew = Vec::new();
        encode_bytes_field(1, &rtype, &mut rew);
        encode_varint_field(2, 1, &mut rew);
        encode_bytes_field(9, &rew, &mut attr_bytes);
    }

    encode_bytes_field(2, &attr_bytes, out);
}

pub fn encode_career_milestone_goal_wire(
    ms: &CareerMilestoneDef,
    progress: u32,
    reached: bool,
    claimed: bool,
    out: &mut Vec<u8>,
) {
    let uuid = career_milestone_uuid(ms.milestone_id);
    let mut id_bytes = Vec::new();
    encode_career_goal_identifier(&uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, out);

    encode_varint_field(2, progress.min(ms.required_points) as u64, out);
    encode_varint_field(3, if reached { 0 } else { 1 }, out);

    let mut label_state = Vec::new();
    encode_varint_field(1, 1, &mut label_state);
    if claimed {
        let ts_proto = GoogleTimestamp {
            seconds: 0,
            nanos: 0,
        };
        encode_bytes_field(2, &ts_proto.encode_proto(), &mut label_state);
    } else {
        encode_varint_field(3, 1, &mut label_state);
    }
    encode_bytes_field(5, &label_state, out);
}

/// Wire encoder for `eve.career.goal.api.GetDefinitionsResponse.Goal`.
pub fn encode_internal_career_goal_definition_wire(def: &CareerGoalDef, out: &mut Vec<u8>) {
    let mut id_bytes = Vec::new();
    encode_career_goal_identifier(&def.uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, out);

    let mut attr_bytes = Vec::new();
    let mut name_id = Vec::new();
    encode_varint_field(1, def.effective_name_message_id() as u64, &mut name_id);
    encode_bytes_field(1, &name_id, &mut attr_bytes);

    let mut desc_id = Vec::new();
    encode_varint_field(1, def.effective_description_message_id() as u64, &mut desc_id);
    encode_bytes_field(2, &desc_id, &mut attr_bytes);

    encode_varint_field(3, def.target.max(1), &mut attr_bytes);
    encode_career_goal_ui_annotations(def, 5, &mut attr_bytes);
    encode_varint_field(6, def.effective_threat() as u64, &mut attr_bytes);
    encode_varint_field(7, def.career_id.clamp(1, 4) as u64, &mut attr_bytes);
    encode_varint_field(8, def.career_points as u64, &mut attr_bytes);
    encode_career_goal_rewards(def, 9, &mut attr_bytes);

    encode_bytes_field(2, &attr_bytes, out);
}

pub fn extract_uuid(dyn_msg: &DynamicQuasarMessage, field_num: u32) -> Option<[u8; 16]> {
    if let Some(DynamicValue::Message(id_msg)) = dyn_msg.get(field_num) {
        if let Some(DynamicValue::Bytes(b)) = id_msg.get(1) {
            if b.len() == 16 {
                let mut arr = [0u8; 16];
                arr.copy_from_slice(b);
                return Some(arr);
            }
        }
    }
    None
}

pub fn extract_target_character(
    dyn_msg: &DynamicQuasarMessage,
    any: &GoogleAny,
    field_num: u32,
    allow_scalar: bool,
) -> Option<u32> {
    if let Some(val) = dyn_msg.get(field_num) {
        match val {
            DynamicValue::Message(m) => {
                if let Some(seq) = m.get(1) {
                    match seq {
                        DynamicValue::Uint32(u) if *u > 0 => return Some(*u),
                        DynamicValue::Uint64(u) if *u > 0 => return Some(*u as u32),
                        DynamicValue::Int32(i) if *i > 0 => return Some(*i as u32),
                        DynamicValue::Int64(i) if *i > 0 => return Some(*i as u32),
                        _ => {}
                    }
                }
            }
            DynamicValue::Uint32(u) if allow_scalar && *u >= 1_000_000 => return Some(*u),
            DynamicValue::Uint64(u) if allow_scalar && *u >= 1_000_000 => return Some(*u as u32),
            DynamicValue::Int32(i) if allow_scalar && *i >= 1_000_000 => return Some(*i as u32),
            DynamicValue::Int64(i) if allow_scalar && *i >= 1_000_000 => return Some(*i as u32),
            _ => {}
        }
    }
    if let Ok(wm) = WireMessage::decode(&any.value) {
        for f in wm.fields.iter().filter(|f| f.field_number == field_num) {
            if let Some(b) = f.value.as_bytes() {
                if let Ok(inner) = WireMessage::decode(b) {
                    if let Some(v) = inner
                        .fields
                        .iter()
                        .find(|sub| sub.field_number == 1)
                        .and_then(|sub| sub.value.as_varint())
                    {
                        if v > 0 {
                            return Some(v as u32);
                        }
                    }
                }
            } else if allow_scalar {
                if let Some(v) = f.value.as_varint() {
                    if v >= 1_000_000 {
                        return Some(v as u32);
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)] #[path = "quasar_career_goals_wire_tests.rs"]
mod quasar_career_goals_wire_tests;

