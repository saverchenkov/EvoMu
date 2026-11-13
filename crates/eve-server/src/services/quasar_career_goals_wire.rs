//! Wire encoders and field extractors for `quasar_career_goals`.

#![forbid(unsafe_code)]

use super::quasar_career_goals::{
    career_milestone_uuid, CareerGoalDef, CareerMilestoneDef, CharacterCareerGoal,
    CAREER_PATH_MESSAGE_IDS,
};
use eve_proto::pb::{self, ProtoBuilder};
use eve_proto::quasar::{
    DynamicQuasarMessage, DynamicValue, GoogleAny, GoogleTimestamp, WireMessage,
};

pub fn encode_career_goal_identifier(uuid: &[u8; 16], out: &mut Vec<u8>) {
    pb::message("eve_public.career.goal.Identifier")
        .set("uuid", uuid)
        .must_encode_into(out);
}

fn build_ui_annotation(type_name: &str, key: &str, value: &str) -> ProtoBuilder {
    pb::message(type_name).set("key", key).set("value", value)
}

fn add_career_goal_rewards(mut attr: ProtoBuilder, def: &CareerGoalDef) -> ProtoBuilder {
    if def.isk_reward > 0 {
        attr = attr.add_with("rewards", |r| {
            r.set("reward_type.isk.units", def.isk_reward)
                .set("reward_label", 1i32) // REWARD_LABEL_ALPHA = 1
        });
    }
    if def.sp_reward > 0.0 {
        attr = attr.add_with("rewards", |r| {
            r.set("reward_type.skill_points", def.sp_reward as u64)
                .set("reward_label", 1i32) // REWARD_LABEL_ALPHA = 1
        });
    }
    attr
}

/// Wire encoder for `eve_public.career.goal.api.GetAllResponse.Goal`.
pub fn encode_career_goal_wire(g: &CharacterCareerGoal, out: &mut Vec<u8>) {
    pb::message("eve_public.career.goal.api.GetAllResponse.Goal")
        .set("goal.uuid", g.uuid)
        .set("progress", g.progress)
        .set("not_completed", !g.completed)
        .set_opt(
            "completed_time",
            g.completed_time
                .map(|ts| GoogleTimestamp::from_unix(ts as i64, 0)),
        )
        .add_with("reward_label_states", |s| {
            s.set("label", 1i32) // REWARD_LABEL_ALPHA
                .when(g.claimed, |s| {
                    s.set(
                        "claimed_time",
                        GoogleTimestamp::from_unix(g.completed_time.unwrap_or(0) as i64, 0),
                    )
                })
                .when(!g.claimed, |s| s.set("not_claimed", true))
        })
        .must_encode_into(out);
}

fn add_career_goal_ui_annotations(
    mut attr: ProtoBuilder,
    annotation_type: &str,
    def: &CareerGoalDef,
) -> ProtoBuilder {
    attr = attr
        .add(
            "ui_annotations",
            build_ui_annotation(annotation_type, "group", def.effective_group()),
        )
        .add(
            "ui_annotations",
            build_ui_annotation(annotation_type, "subtype", "CAREER_PATH_GOAL"),
        )
        .add(
            "ui_annotations",
            build_ui_annotation(annotation_type, "name", &def.name),
        )
        .add(
            "ui_annotations",
            build_ui_annotation(annotation_type, "description", &def.description),
        )
        .add(
            "ui_annotations",
            build_ui_annotation(
                annotation_type,
                "duration",
                &def.effective_duration().to_string(),
            ),
        )
        .add(
            "ui_annotations",
            build_ui_annotation(
                annotation_type,
                "auraTextID",
                &def.effective_aura_text_id().to_string(),
            ),
        )
        .add(
            "ui_annotations",
            build_ui_annotation(
                annotation_type,
                "order",
                &def.effective_order().to_string(),
            ),
        );
    if let Some(link_id) = def.agency_link_text_id.filter(|&id| id > 0) {
        let screen = def
            .agency_screen
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("HOMEPAGE");
        attr = attr
            .add(
                "ui_annotations",
                build_ui_annotation(annotation_type, "agencyLinkTextID", &link_id.to_string()),
            )
            .add(
                "ui_annotations",
                build_ui_annotation(annotation_type, "agencyScreen", screen),
            );
    }
    attr
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
    pb::message("eve_public.career.goal.api.GetDefinitionsResponse.Goal")
        .set("goal.uuid", def.uuid)
        .set_with("attributes", |attr| {
            let attr = attr
                .set("name.sequential", def.effective_name_message_id() as u64)
                .set(
                    "description.sequential",
                    def.effective_description_message_id() as u64,
                )
                .set("progress", 0u32)
                .set("target", def.target.max(1))
                .with(|a| add_career_goal_ui_annotations(a, "eve_public.ui.Annotation", def))
                .set("threat", def.effective_threat() as u32)
                .set("completed", false)
                .set("career", def.career_id.clamp(1, 4))
                .set("career_points", def.career_points)
                .set("claimed", false);
            add_career_goal_rewards(attr, def)
        })
        .must_encode_into(out);
}

pub fn encode_career_milestone_definition_wire(ms: &CareerMilestoneDef, out: &mut Vec<u8>) {
    let uuid = career_milestone_uuid(ms.milestone_id);
    let (name_msg_id, desc_msg_id) = career_milestone_message_ids(ms);
    let (group, subtype) = if ms.career_id.is_some() {
        ("Career Graduation", "CAREER_PATH_GOAL")
    } else {
        ("Overall", "OVERALL_GOAL")
    };

    let anno = "eve_public.ui.Annotation";
    pb::message("eve_public.career.goal.api.GetDefinitionsResponse.Goal")
        .set("goal.uuid", uuid)
        .set_with("attributes", |attr| {
            attr.set("name.sequential", name_msg_id as u64)
                .set("description.sequential", desc_msg_id as u64)
                .set("progress", 0u32)
                .set("target", ms.required_points.max(1))
                .add("ui_annotations", build_ui_annotation(anno, "group", group))
                .add("ui_annotations", build_ui_annotation(anno, "subtype", subtype))
                .add(
                    "ui_annotations",
                    build_ui_annotation(anno, "name", &ms.description),
                )
                .add(
                    "ui_annotations",
                    build_ui_annotation(anno, "description", &ms.description),
                )
                .add("ui_annotations", build_ui_annotation(anno, "duration", "0"))
                .add(
                    "ui_annotations",
                    build_ui_annotation(anno, "auraTextID", &desc_msg_id.to_string()),
                )
                .add(
                    "ui_annotations",
                    build_ui_annotation(anno, "order", &ms.milestone_id.to_string()),
                )
                .set("threat", 1u32)
                .set("completed", false)
                .set(
                    "career",
                    ms.career_id.map(|c| c.clamp(1, 4)).unwrap_or(0),
                )
                .set("career_points", 0u32)
                .set("claimed", false)
                .when(ms.sp_reward > 0.0, |a| {
                    a.add_with("rewards", |r| {
                        r.set("reward_type.skill_points", ms.sp_reward as u64)
                            .set("reward_label", 1i32)
                    })
                })
        })
        .must_encode_into(out);
}

pub fn encode_internal_career_milestone_definition_wire(ms: &CareerMilestoneDef, out: &mut Vec<u8>) {
    let uuid = career_milestone_uuid(ms.milestone_id);
    let (name_msg_id, desc_msg_id) = career_milestone_message_ids(ms);
    let (group, subtype) = if ms.career_id.is_some() {
        ("Career Graduation", "CAREER_PATH_GOAL")
    } else {
        ("Overall", "OVERALL_GOAL")
    };

    let anno = "eve.ui.Annotation";
    pb::message("eve.career.goal.api.GetDefinitionsResponse.Goal")
        .set("goal.uuid", uuid)
        .set_with("attributes", |attr| {
            attr.set("name.sequential", name_msg_id as u64)
                .set("description.sequential", desc_msg_id as u64)
                .set("target", ms.required_points.max(1))
                .add("ui_annotations", build_ui_annotation(anno, "group", group))
                .add("ui_annotations", build_ui_annotation(anno, "subtype", subtype))
                .add(
                    "ui_annotations",
                    build_ui_annotation(anno, "name", &ms.description),
                )
                .add("ui_annotations", build_ui_annotation(anno, "duration", "0"))
                .add(
                    "ui_annotations",
                    build_ui_annotation(anno, "auraTextID", &desc_msg_id.to_string()),
                )
                .add(
                    "ui_annotations",
                    build_ui_annotation(anno, "order", &ms.milestone_id.to_string()),
                )
                .set("threat", 1u32)
                .set(
                    "career",
                    ms.career_id.map(|c| c.clamp(1, 4)).unwrap_or(0),
                )
                .set("career_points", 0u32)
                .when(ms.sp_reward > 0.0, |a| {
                    a.add_with("rewards", |r| {
                        r.set("reward_type.skill_points", ms.sp_reward as u64)
                            .set("reward_label", 1i32)
                    })
                })
        })
        .must_encode_into(out);
}

pub fn encode_career_milestone_goal_wire(
    ms: &CareerMilestoneDef,
    progress: u32,
    reached: bool,
    claimed: bool,
    out: &mut Vec<u8>,
) {
    let uuid = career_milestone_uuid(ms.milestone_id);
    pb::message("eve_public.career.goal.api.GetAllResponse.Goal")
        .set("goal.uuid", uuid)
        .set("progress", progress.min(ms.required_points))
        .set("not_completed", !reached)
        .add_with("reward_label_states", |s| {
            s.set("label", 1i32)
                .when(claimed, |s| {
                    s.set("claimed_time", GoogleTimestamp::from_unix(0, 0))
                })
                .when(!claimed, |s| s.set("not_claimed", true))
        })
        .must_encode_into(out);
}

/// Wire encoder for `eve.career.goal.api.GetDefinitionsResponse.Goal`.
pub fn encode_internal_career_goal_definition_wire(def: &CareerGoalDef, out: &mut Vec<u8>) {
    pb::message("eve.career.goal.api.GetDefinitionsResponse.Goal")
        .set("goal.uuid", def.uuid)
        .set_with("attributes", |attr| {
            let attr = attr
                .set("name.sequential", def.effective_name_message_id() as u64)
                .set(
                    "description.sequential",
                    def.effective_description_message_id() as u64,
                )
                .set("target", def.target.max(1))
                .with(|a| add_career_goal_ui_annotations(a, "eve.ui.Annotation", def))
                .set("threat", def.effective_threat() as u32)
                .set("career", def.career_id.clamp(1, 4))
                .set("career_points", def.career_points);
            add_career_goal_rewards(attr, def)
        })
        .must_encode_into(out);
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

#[cfg(test)]
#[path = "quasar_career_goals_wire_edges_tests.rs"]
mod quasar_career_goals_wire_edges_tests;

