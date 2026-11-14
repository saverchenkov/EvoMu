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
use eve_proto::pb;
use eve_proto::quasar::{
    DynamicValue, GoogleAny, GoogleTimestamp, WireField, WireMessage, WireValue,
    TYPE_GOOGLEAPIS_PREFIX,
};

/// Prefix of the 16-byte goal UUID used for goals backed by the corporation project ledger.
pub const CORP_GOAL_UUID_PREFIX: &[u8; 8] = b"CORPGOAL";

/// `eve_public.goal.State` enum values (goal_pb2).
pub const GOAL_STATE_ACTIVE: u64 = 1;
pub const GOAL_STATE_CLOSED: u64 = 2;
pub const GOAL_STATE_COMPLETED: u64 = 3;

/// Resolves the notice audience for a goal: corporation projects are broadcast to every member
/// of the owning corporation (the client's project UI is corporation-wide); personal goals go to
/// their creator only.
pub fn goal_scope(corp_id: Option<u32>, creator_id: u32) -> NoticeScope {
    match corp_id {
        Some(c) if c > 0 => NoticeScope::Corporation(c),
        _ => NoticeScope::Character(creator_id),
    }
}

/// Extracts the raw serialized bytes of the first length-delimited field `field_number` from a
/// protobuf wire payload (used to forward `CreateRequest.contribution_configuration` verbatim).
pub fn extract_raw_length_delimited_field(wire_bytes: &[u8], field_number: u32) -> Option<Vec<u8>> {
    let msg = WireMessage::decode(wire_bytes).ok()?;
    msg.fields.into_iter().find_map(|f| {
        if f.field_number == field_number {
            if let WireValue::LengthDelimited(bytes) = f.value {
                if !bytes.is_empty() {
                    return Some(bytes);
                }
            }
        }
        None
    })
}

/// Encodes `.eve_public.goal.ContributionConfiguration` from a canonical `(matcher_type, filter_id)`
/// pair so `Attributes.contribution_config` (field 7) is always populated and never causes V24.01
/// `goalMessengerUtil._find_contribution_method_id` to return `None` (`#765`).
pub fn encode_goal_contribution_config(matcher_type: &str, filter_id: Option<i64>) -> Vec<u8> {
    let b = pb::message("eve_public.goal.ContributionConfiguration");
    let positive_id = filter_id.filter(|&v| v > 0).map(|v| v as u64);
    match matcher_type {
        "DeliverItem" => b.set_with("deliver_item", |c| {
            c.add_with("contribution_methods", |m| {
                m.set_opt("delivered_item_type.sequential", positive_id)
                    .set("free_office", true)
            })
        }),
        "DestroyNPCShips" => b.set_with("kill_npc", |c| {
            c.add_with("contribution_methods", |m| match positive_id {
                Some(sid) => m.set("solar_system.sequential", sid),
                None => m.set("free_solar_system", true),
            })
        }),
        "DamageShips" => b.set_with("damage_ship", |c| {
            c.add_with("contribution_methods", |m| {
                m.when(positive_id.is_some(), |m| {
                    m.set("solar_system.sequential", positive_id.unwrap())
                })
                .when(positive_id.is_none(), |m| m.set("free_solar_system", true))
                .set("free_ship_type", true)
                .set("free_organization_type", true)
            })
        }),
        "MineMaterials" | "MineOre" => b.set_with("mine_ore", |c| {
            c.add_with("contribution_methods", |m| {
                m.set("free_solar_system", true)
                    .when(positive_id.is_some(), |m| {
                        m.set("ore.sequential", positive_id.unwrap())
                    })
                    .when(positive_id.is_none(), |m| m.set("free_ore_type", true))
            })
        }),
        "DestroyCapsuleerShips" => b.set_with("destroy_player_ship", |c| {
            c.add_with("contribution_methods", |m| m)
        }),
        "CaptureFWComplexes" => {
            b.set_with("fw_capture", |c| c.add_with("contribution_methods", |m| m))
        }
        "DefendFWComplexes" => {
            b.set_with("fw_defend", |c| c.add_with("contribution_methods", |m| m))
        }
        "RemoteRepairArmor" => b.set_with("remote_armor_repair", |c| {
            c.add_with("contribution_methods", |m| m)
        }),
        "RemoteRepairShield" => b.set_with("remote_shield_repair", |c| {
            c.add_with("contribution_methods", |m| m)
        }),
        "ScanSignatures" => b.set_with("scan_signature", |c| {
            c.add_with("contribution_methods", |m| m)
        }),
        "ManufactureItems" => b.set_with("install_manufacturing_job", |c| {
            c.add_with("contribution_methods", |m| {
                m.set_opt("manufactured_type.sequential", positive_id)
            })
        }),
        "SalvageWrecks" => b.set_with("salvage_wreck", |c| {
            c.add_with("contribution_methods", |m| m)
        }),
        "EarnLoyaltyPoints" => b.set_with("earn_loyalty_points", |c| {
            c.add_with("contribution_methods", |m| m)
        }),
        _ => {
            // `manual` (`ManualContributionConfiguration {}`, field 1)
            b.set_with("manual", |m| m)
        }
    }
    .must_encode()
}

/// Snapshot of the goal attributes carried by `eve_public.goal.Attributes`.
#[derive(Default)]
pub struct GoalAttributesView<'a> {
    pub created_unix: u64,
    pub creator_id: u32,
    pub corp_id: Option<u32>,
    pub name: &'a str,
    pub description: &'a str,
    pub desired: u64,
    pub current: u64,
    /// Optional explicit `eve_public.goal.State`; `None` derives `ACTIVE` vs `COMPLETED`.
    pub state: Option<u64>,
    /// Pre-encoded `.eve_public.goal.ContributionConfiguration` wire bytes (defaults to `manual`).
    pub contribution_config_pb: &'a [u8],
}

/// Encodes `eve_public.goal.Attributes` wire bytes (shared by `CreatedNotice`, `GetResponse`,
/// and `GetAllResponse`).
pub fn encode_goal_attributes(attrs: &GoalAttributesView<'_>) -> Vec<u8> {
    let state = attrs.state.unwrap_or_else(|| {
        if attrs.current >= attrs.desired {
            GOAL_STATE_COMPLETED
        } else {
            GOAL_STATE_ACTIVE
        }
    });
    let created_ts = GoogleTimestamp::from_unix(attrs.created_unix.max(1) as i64, 0);
    let fallback_cfg;
    let cfg_bytes = if !attrs.contribution_config_pb.is_empty() {
        attrs.contribution_config_pb
    } else {
        fallback_cfg = encode_goal_contribution_config("Manual", None);
        &fallback_cfg
    };
    let corp = attrs.corp_id.filter(|&c| c > 0);

    pb::message("eve_public.goal.Attributes")
        .set("created", created_ts)
        .set("user_input_name", attrs.name)
        .set("user_input_description", attrs.description)
        .when(attrs.creator_id > 0, |b| {
            b.set("creator.sequential", attrs.creator_id)
        })
        .set("progress.desired", attrs.desired)
        .set("progress.current", attrs.current)
        .set("state", state as i32)
        .set_raw_message("contribution_config", cfg_bytes)
        .when(corp.is_some(), |b| {
            let cid = corp.unwrap();
            // Attributes.organization = Organization { corporation = 1 }
            // V24.01 goal_formatter also reads assigner (19) and assignee (20).
            b.set("organization.corporation.sequential", cid)
                .set("assigner.corporation.sequential", cid)
                .set("assignee.corporation.sequential", cid)
        })
        .when(state == GOAL_STATE_ACTIVE, |b| b.set("not_finished", true))
        .when(state != GOAL_STATE_ACTIVE, |b| {
            b.set("finished", created_ts)
        })
        .set("no_due_timestamp", true)
        .set("unlimited", true)
        .set("contribution_unlimited", true)
        .set("default", true)
        .must_encode()
}

/// `eve_public.goal.api.CreatedNotice { id = 1, goal = 2 }`.
pub fn goal_created(uuid: &[u8; 16], attrs: &GoalAttributesView<'_>) -> GoogleAny {
    pb::message("eve_public.goal.api.CreatedNotice")
        .set("id.uuid", uuid)
        .set_raw_message("goal", encode_goal_attributes(attrs))
        .must_to_any()
}

/// `eve_public.goal.api.DeletedNotice { goal = 1 }`.
pub fn goal_deleted(uuid: &[u8; 16]) -> GoogleAny {
    pb::message("eve_public.goal.api.DeletedNotice")
        .set("goal.uuid", uuid)
        .must_to_any()
}

/// `eve_public.goal.api.ClosedNotice { goal = 1, closer = 2 }`.
pub fn goal_closed(uuid: &[u8; 16], closer_id: u32) -> GoogleAny {
    pb::message("eve_public.goal.api.ClosedNotice")
        .set("goal.uuid", uuid)
        .set("closer.sequential", closer_id)
        .must_to_any()
}

/// `eve_public.goal.api.CompletedNotice { goal = 1 }`.
pub fn goal_completed(uuid: &[u8; 16]) -> GoogleAny {
    pb::message("eve_public.goal.api.CompletedNotice")
        .set("goal.uuid", uuid)
        .must_to_any()
}

/// `eve_public.goal.api.ProgressedNotice { goal = 1, current_progress = 2 }`.
pub fn goal_progressed(uuid: &[u8; 16], current: u64) -> GoogleAny {
    pb::message("eve_public.goal.api.ProgressedNotice")
        .set("goal.uuid", uuid)
        .set("current_progress", current)
        .must_to_any()
}

/// `eve_public.goal.api.NameChangedNotice { goal = 1, admin_designated_new_name = 2 }`.
pub fn goal_name_changed(uuid: &[u8; 16], new_name: &str) -> GoogleAny {
    pb::message("eve_public.goal.api.NameChangedNotice")
        .set("goal.uuid", uuid)
        .set("admin_designated_new_name", new_name)
        .must_to_any()
}

/// `eve_public.goal.api.DescriptionChangedNotice { goal = 1, admin_designated_new_description = 2 }`.
pub fn goal_description_changed(uuid: &[u8; 16], new_description: &str) -> GoogleAny {
    pb::message("eve_public.goal.api.DescriptionChangedNotice")
        .set("goal.uuid", uuid)
        .set("admin_designated_new_description", new_description)
        .must_to_any()
}

/// `eve_public.goal.contribution.api.ContributedNotice
///  { timestamp = 1, contributor = 2, goal = 3, previous_progress = 4, current_progress = 5 }`.
pub fn goal_contributed(
    uuid: &[u8; 16],
    contributor_id: u32,
    previous: u64,
    current: u64,
    now_unix: u64,
) -> GoogleAny {
    pb::message("eve_public.goal.contribution.api.ContributedNotice")
        .set("timestamp", GoogleTimestamp::from_unix(now_unix as i64, 0))
        .set("contributor.sequential", contributor_id)
        .set("goal.uuid", uuid)
        .set("previous_progress", previous)
        .set("current_progress", current)
        .must_to_any()
}

/// `eve_public.tale.api.StartedNotice` / `StoppedNotice`
/// `{ tale = 1, spawntemplate = 2, solar_system = 3 }`.
pub fn tale_lifecycle(
    started: bool,
    tale_id: u64,
    template_id: u64,
    solar_system_id: u64,
) -> GoogleAny {
    let name = if started {
        "eve_public.tale.api.StartedNotice"
    } else {
        "eve_public.tale.api.StoppedNotice"
    };
    pb::message(name)
        .set("tale.sequential", tale_id)
        .set("spawntemplate.sequential", template_id)
        .set("solar_system.sequential", solar_system_id)
        .must_to_any()
}

/// Audience for tale lifecycle notices. The client's tale service keeps a universe-wide
/// "active tales by template" cache (`get_global_world_event_tales`) that it invalidates on
/// either notice, so tale lifecycle is broadcast to every subscribed session.
pub fn tale_scope() -> NoticeScope {
    NoticeScope::Broadcast
}

fn format_opportunity_id_value(v: &DynamicValue) -> String {
    match v {
        DynamicValue::String(s) => s.clone(),
        DynamicValue::Bytes(b) => b.iter().map(|x| format!("{x:02x}")).collect(),
        DynamicValue::Uint64(u) => u.to_string(),
        DynamicValue::Int64(i) => i.to_string(),
        DynamicValue::Uint32(u) => u.to_string(),
        DynamicValue::Int32(i) => i.to_string(),
        DynamicValue::Bool(b) => b.to_string(),
        DynamicValue::Enum(e) => e.to_string(),
        DynamicValue::Message(m) => m
            .fields
            .iter()
            .map(|(k, fv)| format!("{k}={}", format_opportunity_id_value(fv)))
            .collect::<Vec<_>>()
            .join(","),
        DynamicValue::Repeated(items) => items
            .iter()
            .map(format_opportunity_id_value)
            .collect::<Vec<_>>()
            .join(","),
        other => format!("{other:?}"),
    }
}

/// Consumes client-published `eve_public.opportunity.api.{Viewed,Tracked,Untracked,Completed,Discovered}`
/// events from `publicGatewaySvc.PublishEvent` / `PublishEventPayload`, recording only UI
/// `tracked`, `viewed`, and `history` metadata without advancing server opportunity or achievement completion.
pub fn handle_opportunity_event(
    manager: &crate::services::quasar_progression::ProgressionManagers,
    descriptors: &eve_proto::quasar::QuasarDescriptorRegistry,
    session: &eve_machonet::SessionState,
    any: &GoogleAny,
    _now_unix: u64,
) {
    let event_name = eve_proto::quasar::QuasarDescriptorRegistry::normalize_type_name(&any.type_url);
    if !matches!(
        event_name,
        "eve_public.opportunity.api.Viewed"
            | "eve_public.opportunity.api.Tracked"
            | "eve_public.opportunity.api.Untracked"
            | "eve_public.opportunity.api.Completed"
            | "eve_public.opportunity.api.Discovered"
    ) {
        return;
    }
    let Some(cid) = session.character_id.and_then(|c| u32::try_from(c).ok()).filter(|&c| c > 0) else {
        return;
    };
    let Ok(dyn_msg) = descriptors.decode_dynamic(&any.type_url, &any.value) else { return };
    let opp_key = if event_name.ends_with(".Discovered") {
        let Some(DynamicValue::Message(disc_opp)) = dyn_msg.get(1) else { return };
        let tag = match disc_opp.get(1) {
            Some(DynamicValue::Message(m)) => match m.get(1) {
                Some(DynamicValue::String(s)) if !s.trim().is_empty() => s.clone(),
                Some(DynamicValue::Uint64(v)) if *v > 0 => v.to_string(),
                Some(DynamicValue::Int64(v)) if *v > 0 => v.to_string(),
                _ => String::new(),
            },
            Some(DynamicValue::String(s)) => s.clone(),
            _ => String::new(),
        };
        if tag.is_empty() { return; }
        format!("discovered:{tag}")
    } else {
        if let Some(DynamicValue::Message(ch_msg)) = dyn_msg.get(1) {
            let req_cid = match ch_msg.get(1) {
                Some(DynamicValue::Uint64(v)) => u32::try_from(*v).ok(),
                Some(DynamicValue::Int64(v)) if *v > 0 => u32::try_from(*v).ok(),
                Some(DynamicValue::Uint32(v)) => Some(*v),
                Some(DynamicValue::Int32(v)) if *v > 0 => u32::try_from(*v).ok(),
                _ => None,
            };
            if req_cid.filter(|&c| c > 0).is_some_and(|rc| rc != cid) { return; }
        }
        let Some(DynamicValue::Message(opp_msg)) = dyn_msg.get(2) else { return };
        let Some(variant_num) = (1u32..=8).find(|&f| opp_msg.get(f).is_some()) else { return };
        let Some(variant_val) = opp_msg.get(variant_num) else { return };
        let id_str = format_opportunity_id_value(variant_val);
        if id_str.is_empty() { return; }
        format!("v{variant_num}:{id_str}")
    };
    if let Some(ref db) = manager.extra.db {
        db.with_mut_snapshot(|snap| {
            let mut state: serde_json::Value = snap.quasar_state_blobs.get("quasar_opportunities").and_then(|j| serde_json::from_str(j).ok()).unwrap_or_else(|| serde_json::json!({}));
            if !state.is_object() { state = serde_json::json!({}); }
            if let Some(obj) = state.as_object_mut() {
                let ch_entry = obj.entry(cid.to_string()).or_insert_with(|| serde_json::json!({"tracked": [], "viewed": [], "history": [], "completed": []}));
                if let Some(eobj) = ch_entry.as_object_mut() {
                    eobj.entry("tracked").or_insert_with(|| serde_json::json!([]));
                    eobj.entry("viewed").or_insert_with(|| serde_json::json!([]));
                    eobj.entry("history").or_insert_with(|| serde_json::json!([]));
                    eobj.entry("completed").or_insert_with(|| serde_json::json!([]));
                }
                if event_name.ends_with(".Tracked") {
                    if let Some(arr) = ch_entry.get_mut("tracked").and_then(|v| v.as_array_mut()) {
                        if !arr.iter().any(|x| x.as_str() == Some(&opp_key)) {
                            if arr.len() >= 256 { arr.remove(0); }
                            arr.push(serde_json::Value::String(opp_key.clone()));
                        }
                    }
                } else if event_name.ends_with(".Untracked") {
                    if let Some(arr) = ch_entry.get_mut("tracked").and_then(|v| v.as_array_mut()) {
                        arr.retain(|x| x.as_str() != Some(&opp_key));
                    }
                } else if event_name.ends_with(".Viewed") {
                    if let Some(arr) = ch_entry.get_mut("viewed").and_then(|v| v.as_array_mut()) {
                        if !arr.iter().any(|x| x.as_str() == Some(&opp_key)) {
                            if arr.len() >= 256 { arr.remove(0); }
                            arr.push(serde_json::Value::String(opp_key.clone()));
                        }
                    }
                }
                if let Some(hist) = ch_entry.get_mut("history").and_then(|v| v.as_array_mut()) {
                    if hist.len() >= 256 { hist.remove(0); }
                    hist.push(serde_json::json!({"event": event_name, "key": opp_key}));
                }
            }
            snap.quasar_state_blobs.insert("quasar_opportunities".into(), state.to_string());
        });
    }
}

/// Records server-observed gameplay progress and completion for SDE opportunities
/// (`achievements.static` + `achievementevents.fsdbinary`) directly in `quasar_opportunities`,
/// decoupled from `quasar_achievements` and unreachable from client `PublishEventPayload` calls.
pub fn record_server_opportunity_event(
    db: &eve_db::WorldRepository,
    definitions: &[crate::services::quasar_achievements::AchievementDef],
    character_id: u32,
    event_names: &[&str],
    delta: u64,
    now_unix: u64,
) {
    if character_id == 0 || delta == 0 || event_names.is_empty() {
        return;
    }
    let matching_defs: Vec<&crate::services::quasar_achievements::AchievementDef> = definitions
        .iter()
        .filter(|d| {
            !d.is_client_achievement
                && !d.conditions.is_empty()
                && d.conditions.iter().any(|c| event_names.contains(&c.event_name.as_str()))
        })
        .collect();
    if matching_defs.is_empty() {
        return;
    }
    let needs_write = db.with_snapshot(|snap| {
        let Some(raw) = snap.quasar_state_blobs.get("quasar_opportunities") else {
            return true;
        };
        let Ok(state) = serde_json::from_str::<serde_json::Value>(raw) else {
            return true;
        };
        let ch_key = character_id.to_string();
        let Some(eobj) = state.get(&ch_key).and_then(|v| v.as_object()) else {
            return true;
        };
        if !eobj.contains_key("completed")
            || !eobj.contains_key("progress")
            || !eobj.contains_key("completed_at")
        {
            return true;
        }
        let Some(prog_obj) = eobj.get("progress").and_then(|v| v.as_object()) else {
            return true;
        };
        let Some(comp_arr) = eobj.get("completed").and_then(|v| v.as_array()) else {
            return true;
        };
        for def in &matching_defs {
            for cond in &def.conditions {
                if event_names.contains(&cond.event_name.as_str()) {
                    let k = format!("{}:{}", def.id, cond.condition_id);
                    let prev = prog_obj.get(&k).and_then(|v| v.as_u64()).unwrap_or(0);
                    let next = prev.saturating_add(delta).min(cond.target_value);
                    if next != prev || !prog_obj.contains_key(&k) {
                        return true;
                    }
                }
            }
            let all_done = def.conditions.iter().all(|c| {
                let k = format!("{}:{}", def.id, c.condition_id);
                prog_obj.get(&k).and_then(|v| v.as_u64()).unwrap_or(0) >= c.target_value
            });
            if all_done && !comp_arr.iter().any(|x| x.as_u64() == Some(u64::from(def.id))) {
                return true;
            }
        }
        false
    });
    if !needs_write {
        return;
    }
    db.with_mut_snapshot(|snap| {
        let mut state: serde_json::Value = snap
            .quasar_state_blobs
            .get("quasar_opportunities")
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        if !state.is_object() {
            state = serde_json::json!({});
        }
        if let Some(obj) = state.as_object_mut() {
            let ch_entry = obj
                .entry(character_id.to_string())
                .or_insert_with(|| serde_json::json!({"tracked": [], "viewed": [], "history": [], "completed": [], "progress": {}, "completed_at": {}}));
            if let Some(eobj) = ch_entry.as_object_mut() {
                eobj.entry("completed").or_insert_with(|| serde_json::json!([]));
                eobj.entry("progress").or_insert_with(|| serde_json::json!({}));
                eobj.entry("completed_at").or_insert_with(|| serde_json::json!({}));
                for def in &matching_defs {
                    if let Some(prog_obj) = eobj.get_mut("progress").and_then(|v| v.as_object_mut()) {
                        for cond in &def.conditions {
                            if event_names.contains(&cond.event_name.as_str()) {
                                let k = format!("{}:{}", def.id, cond.condition_id);
                                let prev = prog_obj.get(&k).and_then(|v| v.as_u64()).unwrap_or(0);
                                let next = prev.saturating_add(delta).min(cond.target_value);
                                prog_obj.insert(k, serde_json::json!(next));
                            }
                        }
                    }
                    let all_done = eobj
                        .get("progress")
                        .and_then(|v| v.as_object())
                        .is_some_and(|prog_obj| {
                            def.conditions.iter().all(|c| {
                                let k = format!("{}:{}", def.id, c.condition_id);
                                prog_obj.get(&k).and_then(|v| v.as_u64()).unwrap_or(0) >= c.target_value
                            })
                        });
                    if all_done {
                        if let Some(comp_arr) = eobj.get_mut("completed").and_then(|v| v.as_array_mut()) {
                            let id_u64 = u64::from(def.id);
                            if !comp_arr.iter().any(|x| x.as_u64() == Some(id_u64)) {
                                comp_arr.push(serde_json::json!(def.id));
                                if let Some(cat_obj) = eobj.get_mut("completed_at").and_then(|v| v.as_object_mut()) {
                                    cat_obj.insert(def.id.to_string(), serde_json::json!(now_unix));
                                }
                            }
                        }
                    }
                }
            }
        }
        snap.quasar_state_blobs
            .insert("quasar_opportunities".into(), state.to_string());
    });
}

/// Filters completed PvE encounters against an active tale template's site/dungeon category and SDE definition
/// so unrelated combat anomalies or exploration hacking sites do not advance mismatched world-event tales.
pub fn tale_matches_completed_encounter(
    sde: Option<&eve_sde::SdeRegistry>,
    sde_event_tale_templates: &std::collections::HashSet<u32>,
    tale_template_id: u32,
    site_category: &str,
    dungeon_id: u32,
) -> bool {
    let inc_tpl = crate::services::pve_incursion_sites::INCURSION_TALE_TEMPLATE_ID as u32;
    let dng_arch = sde.and_then(|s| s.dungeons.get(&tale_template_id)).and_then(|d| d.archetype_id);
    match site_category {
        "Incursion" => {
            tale_template_id == inc_tpl
                || (dungeon_id != 0 && tale_template_id == dungeon_id)
                || matches!(dng_arch, Some(61..=64))
        }
        "Triglavian" => (dungeon_id != 0 && tale_template_id == dungeon_id) || dng_arch == Some(65),
        "Data" | "Relic" | "Ghost" | "SleeperCache" | "WormholeCombat" | "DrifterHive" | "Homefront" => {
            dungeon_id != 0 && tale_template_id == dungeon_id
        }
        "CombatAnomaly" | "CombatSignature" | "DED" | "Expedition" | "Event" | "EventSite" => {
            if tale_template_id == inc_tpl || matches!(dng_arch, Some(27..=30 | 38 | 44 | 61..=65)) {
                return dungeon_id != 0 && tale_template_id == dungeon_id;
            }
            (dungeon_id != 0 && tale_template_id == dungeon_id)
                || sde_event_tale_templates.contains(&tale_template_id)
        }
        _ => false,
    }
}

/// Publishes `eve_public.communitytrack.api.ProgressedNotice` directly to subscribed Quasar gateway clients
/// and flushes any thread-local queued notices so worker threads never strand notices in TLS.
pub fn publish_community_track_progressed_notices(
    gateway: &crate::quasar_gateway::PublicGatewayRouter,
    tracks: &[super::quasar_progression_extra::CommunityTrackState],
    now_unix: u64,
) {
    for tr in tracks {
        // Synthetic gateway notice `eve_public.communitytrack.api.ProgressedNotice` is absent
        // from V24.01 `quasar_descriptors.sqlite` (only `eve.communitytrack.api.Progressed` exists).
        let not_bytes = WireMessage {
            fields: vec![WireField {
                field_number: 1,
                value: WireValue::Varint(tr.track_id as u64),
            }],
        }
        .encode();
        let notice = GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.communitytrack.api.ProgressedNotice"),
            value: not_bytes,
        };
        let _ = gateway.publish_notice(NoticeScope::Broadcast, notice, now_unix);
    }
    for (scope, payload, ts) in crate::services::quasar_social_chat::drain_local_notices() {
        let _ = gateway.publish_notice(scope, payload, ts);
    }
}

pub fn complete_system_tale_encounter(
    mgr: &super::quasar_progression_extra::ProgressionExtraManager,
    solar_system_id: u32,
    site_category: &str,
    dungeon_id: u32,
    delta: u64,
    now_unix: u64,
    gateway: &crate::quasar_gateway::PublicGatewayRouter,
) -> Vec<super::quasar_progression_extra::CommunityTrackState> {
    let mut matching_templates: std::collections::BTreeSet<u32> = mgr
        .active_tales
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .filter(|&&(tpl_id, sys_id, _)| {
            sys_id == solar_system_id
                && tale_matches_completed_encounter(mgr.sde.as_deref(), &mgr.sde_event_tale_templates, tpl_id, site_category, dungeon_id)
        })
        .map(|&(tpl_id, _, _)| tpl_id)
        .collect();
    if site_category == "Incursion" {
        matching_templates.insert(crate::services::pve_incursion_sites::INCURSION_TALE_TEMPLATE_ID as u32);
    }
    let mut all_progressed = Vec::new();
    if !matching_templates.is_empty() {
        let mut map = mgr.community_tracks.write().unwrap_or_else(|e| e.into_inner());
        for track in map.values_mut() {
            if track.active && track.tale_templates.iter().any(|tpl| matching_templates.contains(tpl)) {
                track.current_progress = track.current_progress.saturating_add(delta).min(track.target_progress);
                all_progressed.push(track.clone());
            }
        }
    }
    if !all_progressed.is_empty() {
        mgr.save_to_db();
        publish_community_track_progressed_notices(gateway, &all_progressed, now_unix);
    }
    all_progressed
}

#[cfg(test)]
#[path = "quasar_goal_tale_notices_tests.rs"]
mod quasar_goal_tale_notices_tests;
#[cfg(test)]
#[path = "quasar_goal_tale_notices_edges_tests.rs"]
mod quasar_goal_tale_notices_edges_tests;

