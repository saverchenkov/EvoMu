//! `quasar_freelance_defs` — V24 Freelance Item Delivery, SDE Contribution Method Serialization & Cursor Pagination.

use super::quasar_social_chat::{ctx_now_unix, ctx_queue_notice};
use super::quasar_social_freelance::QuasarFreelanceState;
use super::social_corp_fleet::social_corp_projects as corp_projects;
use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_UNAUTHORIZED,
};
use eve_proto::pb::{self, ProtoBuilder};
use eve_proto::quasar::{DynamicValue, GoogleAny, WireField, WireMessage, WireValue};
use std::sync::Arc;

pub fn encode_uuid_field(field_num: u32, uuid: &[u8], out: &mut Vec<u8>) {
    let inner = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(uuid.to_vec()),
        }],
    }
    .encode();
    out.extend_from_slice(
        &WireMessage {
            fields: vec![WireField {
                field_number: field_num,
                value: WireValue::LengthDelimited(inner),
            }],
        }
        .encode(),
    );
}

pub fn encode_char_id_field(field_num: u32, cid: u32, out: &mut Vec<u8>) {
    let inner = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::Varint(u64::from(cid)),
        }],
    }
    .encode();
    out.extend_from_slice(
        &WireMessage {
            fields: vec![WireField {
                field_number: field_num,
                value: WireValue::LengthDelimited(inner),
            }],
        }
        .encode(),
    );
}

pub fn extract_uuid(val: Option<&DynamicValue>) -> Option<Vec<u8>> {
    match val {
        Some(DynamicValue::Message(sub)) => extract_uuid(sub.get(1)),
        Some(DynamicValue::Bytes(b)) => Some(b.clone()),
        _ => None,
    }
}

pub fn extract_currency_isk(val: Option<&DynamicValue>) -> Option<i64> {
    match val? {
        DynamicValue::Message(cm) => match cm.get(1)? {
            DynamicValue::Uint64(v) => i64::try_from(*v).ok(),
            DynamicValue::Int64(v) | DynamicValue::Sint64(v) => Some(*v),
            DynamicValue::Uint32(v) => Some(i64::from(*v)),
            DynamicValue::Int32(v) | DynamicValue::Sint32(v) => Some(i64::from(*v)),
            _ => None,
        },
        DynamicValue::Uint64(v) => i64::try_from(*v).ok(),
        DynamicValue::Int64(v) | DynamicValue::Sint64(v) => Some(*v),
        DynamicValue::Uint32(v) => Some(i64::from(*v)),
        DynamicValue::Int32(v) | DynamicValue::Sint32(v) => Some(i64::from(*v)),
        _ => None,
    }
}

pub fn paginate_slice<T: Clone>(
    page_val: Option<&DynamicValue>,
    items: &[T],
) -> (Vec<T>, Option<String>) {
    let Some(DynamicValue::Message(pm)) = page_val else {
        return (items.to_vec(), None);
    };
    let size = pm
        .get(1)
        .and_then(|v| match v {
            DynamicValue::Uint32(n) if *n > 0 => Some(*n as usize),
            DynamicValue::Int32(n) if *n > 0 => Some(*n as usize),
            DynamicValue::Uint64(n) if *n > 0 => Some(*n as usize),
            DynamicValue::Int64(n) if *n > 0 => Some(*n as usize),
            _ => None,
        })
        .unwrap_or(50);
    let offset = pm
        .get(2)
        .and_then(|v| match v {
            DynamicValue::String(s) if !s.trim().is_empty() => s.trim().parse::<usize>().ok(),
            DynamicValue::Bytes(b) if !b.is_empty() => {
                std::str::from_utf8(b).ok()?.trim().parse::<usize>().ok()
            }
            _ => None,
        })
        .unwrap_or(0)
        .min(items.len());
    let end = offset.saturating_add(size).min(items.len());
    let next_tok = (end < items.len()).then(|| end.to_string());
    (items[offset..end].to_vec(), next_tok)
}

pub fn encode_next_page(field_num: u32, next_token: Option<&str>, out: &mut Vec<u8>) {
    if let Some(tok) = next_token {
        let np = WireMessage {
            fields: vec![WireField {
                field_number: 1,
                value: WireValue::LengthDelimited(tok.as_bytes().to_vec()),
            }],
        }
        .encode();
        out.extend_from_slice(
            &WireMessage {
                fields: vec![WireField {
                    field_number: field_num,
                    value: WireValue::LengthDelimited(np),
                }],
            }
            .encode(),
        );
    }
}

pub fn register_item_delivery_and_definitions(
    router: &PublicGatewayRouter,
    state: Arc<QuasarFreelanceState>,
) {
    // 18. DeliverRequest -> DeliverResponse (Item Delivery Contribution Method)
    let state_deliver = Arc::clone(&state);
    let sde_deliver = Arc::clone(&router.sde);
    router.register_handler(
        "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_UNAUTHORIZED, "Unauthenticated".into()));
            };
            let uuid = extract_uuid(dyn_msg.get(1))
                .ok_or((STATUS_BAD_REQUEST, "Missing project UUID".into()))?;
            let now_u = ctx_now_unix(ctx);
            state_deliver.refresh_from_db(now_u);

            let (target_type_id, batch_size, corpid, rem_progress, target_prog_total) = {
                let projects = state_deliver.projects.read().unwrap_or_else(|e| e.into_inner());
                let Some(p) = projects.get(&uuid) else {
                    return Err((STATUS_NOT_FOUND, "Freelance project not found".into()));
                };
                if p.state != 1 || !p.committed_participants.contains(&cid) {
                    return Err((
                        STATUS_BAD_REQUEST,
                        "Project not active or caller not committed".into(),
                    ));
                };
                let rem = (p.target_progress - p.current_progress).max(0);
                if rem <= 0 || p.required_type_id == 0 {
                    return Err((
                        STATUS_BAD_REQUEST,
                        "Project target progress already reached".into(),
                    ));
                }
                (
                    p.required_type_id,
                    p.batch_size.max(1),
                    p.owner_corp_id,
                    rem,
                    p.target_progress,
                )
            };
            let Some(pid) = corp_projects::freelance_uuid_to_project_id(&uuid) else {
                return Err((STATUS_NOT_FOUND, "Freelance project not found".into()));
            };
            let Some(ref db) = state_deliver.db else {
                return Err((STATUS_BAD_REQUEST, "Database not available".into()));
            };
            let is_item_delivery = db.with_snapshot(|s| {
                s.alliance_war_state.project_meta.get(&pid).is_some_and(|m| m.0 == "DeliverItem")
            });
            if !is_item_delivery {
                // V24 `DeliverResponse.project_does_not_allow_item_delivery = 12`.
                return Ok(pb::message(
                    "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverResponse",
                )
                .set_with("project_does_not_allow_item_delivery", |v| v)
                .must_to_any());
            }
            let req_qty = dyn_msg
                .get(4)
                .and_then(|v| match v {
                    DynamicValue::Uint64(u) => Some(*u as i64),
                    DynamicValue::Int64(i) => Some(*i),
                    DynamicValue::Uint32(u) => Some(*u as i64),
                    DynamicValue::Int32(i) => Some(*i as i64),
                    _ => None,
                })
                .filter(|q| *q > 0)
                .ok_or((STATUS_BAD_REQUEST, "DeliverRequest quantity is required".into()))?
                .max(batch_size)
                .min(rem_progress);

            // V24 DeliverRequest: 2 = delivery_location {1 station | 2 structure}, 3 = item (sequential id),
            // 5 = item_location {1 item (container), 2 flag}.
            let seq_of = |v: Option<&DynamicValue>| -> Option<i64> {
                match v {
                    Some(DynamicValue::Message(m)) => match m.get(1) {
                        Some(DynamicValue::Uint64(u)) => i64::try_from(*u).ok(),
                        Some(DynamicValue::Int64(i)) => Some(*i),
                        Some(DynamicValue::Uint32(u)) => Some(i64::from(*u)),
                        Some(DynamicValue::Int32(i)) => Some(i64::from(*i)),
                        _ => None,
                    },
                    // Identifier sub-messages whose schema is not resolved arrive as raw wire bytes.
                    Some(DynamicValue::Bytes(b)) => eve_proto::quasar::WireMessage::decode(b)
                        .ok()?
                        .fields
                        .iter()
                        .find(|f| f.field_number == 1)
                        .and_then(|f| f.value.as_varint())
                        .and_then(|v| i64::try_from(v).ok()),
                    _ => None,
                }
            };
            let delivery_loc = match dyn_msg.get(2) {
                Some(DynamicValue::Message(dl)) => seq_of(dl.get(1)).or_else(|| seq_of(dl.get(2))),
                _ => None,
            };
            let item_id = seq_of(dyn_msg.get(3));
            let (item_container, item_flag) = match dyn_msg.get(5) {
                Some(DynamicValue::Message(l)) => (
                    seq_of(l.get(1)),
                    l.get(2).and_then(|f| match f {
                        DynamicValue::Message(m) => seq_of(m.get(1)),
                        DynamicValue::Enum(e) => Some(i64::from(*e)),
                        DynamicValue::Uint32(u) => Some(i64::from(*u)),
                        DynamicValue::Int32(i) => Some(i64::from(*i)),
                        _ => None,
                    }),
                ),
                _ => (None, None),
            };

            let mut delivered_units = 0i64;
            let mut consumed_loc = 0i64;
            let mut found = false;
            let rejection: Option<u32> = db.with_mut_snapshot(|snap| {
                let Some(char_loc) = snap
                    .characters
                    .get(&(cid as i64))
                    .and_then(|c| c.station_id.or(c.structure_id))
                else {
                    return Some(5); // character_not_docked
                };
                if let Some(dl) = delivery_loc {
                    if !sde_deliver.stations.contains_key(&dl) && !snap.structures.contains_key(&dl) {
                        return Some(3); // delivery_location_does_not_exist
                    }
                    if dl != char_loc {
                        return Some(5); // character_not_docked (at the delivery location)
                    }
                }
                let max_char_rem = snap
                    .alliance_war_state
                    .project_details
                    .get(&pid)
                    .and_then(|d| {
                        let prev = d.char_contributions.get(&(cid as i64)).copied().unwrap_or(0);
                        d.max_per_character.map(|cap| cap.saturating_sub(prev) as i64)
                    })
                    .unwrap_or(i64::MAX);
                let allowed_qty = req_qty.min(max_char_rem);
                if allowed_qty < batch_size {
                    return Some(10); // quantity_too_high
                }
                let chosen = if let Some(iid) = item_id {
                    let Some(item) = snap.items.get(&iid) else {
                        return Some(8); // item_not_found_at_location
                    };
                    if snap.is_item_locked_in_contract_or_job(iid) {
                        return Some(8); // item locked in contract/market escrow
                    }
                    if item.owner_id != cid as i64 {
                        return Some(6); // item_not_owned_by_deliverer
                    }
                    if item.type_id != target_type_id
                        || item.singleton
                        || snap.items.values().any(|c| c.location_id == iid)
                    {
                        return Some(9); // invalid_item_type
                    }
                    if item_container.is_some_and(|c| c != item.location_id)
                        || item_flag.is_some_and(|f| f != item.flag as i64)
                    {
                        return Some(7); // invalid_item_location
                    }
                    if item.location_id != char_loc || item.flag != eve_db::InventoryFlag::Hangar {
                        return Some(8);
                    }
                    if snap.is_item_or_ancestor_locked_or_impounded(iid)
                        || snap.is_container_non_empty(iid)
                        || item.singleton
                    {
                        return Some(7);
                    }
                    Some(iid)
                } else {
                    snap.items
                        .values()
                        .filter(|item| {
                            item.owner_id == cid as i64
                                && !snap.is_item_or_ancestor_locked_or_impounded(item.item_id)
                                && item.type_id == target_type_id
                                && !item.singleton
                                && !snap.is_container_non_empty(item.item_id)
                                && item.quantity >= (batch_size as i32)
                                && item.flag == eve_db::InventoryFlag::Hangar
                                && item.location_id == char_loc
                        })
                        .map(|item| item.item_id)
                        .min()
                };
                let Some(iid) = chosen else {
                    return None;
                };
                if let Some(avail_qty) = snap.items.get(&iid).map(|it| it.quantity) {
                    let take = ((avail_qty.min(allowed_qty as i32) as i64) / batch_size) * batch_size;
                    if take > 0 {
                        let opts = eve_db::ConsumeExactOptions {
                            required_location_id: Some(char_loc),
                            required_flag: Some(eve_db::InventoryFlag::Hangar),
                            ..eve_db::ConsumeExactOptions::for_character_stack(cid as i64)
                        };
                        if snap.consume_exact_item_quantity(iid, take as i32, opts).is_ok() {
                            delivered_units = take;
                            consumed_loc = char_loc;
                            found = true;
                        }
                    }
                }
                None
            });
            if let Some(variant) = rejection {
                let variant_field = match variant {
                    3 => "delivery_location_does_not_exist",
                    4 => "delivery_location_not_in_full_power",
                    5 => "character_not_docked",
                    6 => "item_not_owned_by_deliverer",
                    7 => "invalid_item_location",
                    8 => "item_not_found_at_location",
                    9 => "invalid_item_type",
                    10 => "quantity_too_high",
                    11 => "character_not_committed",
                    12 => "project_does_not_allow_item_delivery",
                    _ => "project_does_not_exist",
                };
                return Ok(pb::message(
                    "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverResponse",
                )
                .set_with(variant_field, |v| v)
                .must_to_any());
            }
            if !found {
                ctx_queue_notice(
                    NoticeScope::Character(cid),
                    pb::message(
                        "eve_public.freelance.contributionmethod.itemdelivery.api.ContributionFailedNotice",
                    )
                    .set("identifier.uuid", uuid.clone())
                    .must_to_any(),
                    now_u,
                );
                return Err((
                    STATUS_BAD_REQUEST,
                    format!(
                        "Failed precondition: no items of type {target_type_id} available to deliver"
                    ),
                ));
            }

            let mut completed = false;
            let mut total_prog = 0i64;
            let mut char_total_prog = 0u64;
            let mut total_contributors = 1usize;
            let applied = db.with_mut_snapshot(|snap| {
                let is_active = snap
                    .alliance_war_state
                    .project_details
                    .get(&pid)
                    .is_some_and(|d| {
                        d.status == "Active" && d.committed_participants.contains(&(cid as i64))
                    });
                if !is_active || !snap.corp_projects.contains_key(&pid) {
                    return false;
                }
                if let Some(p_mut) = snap.corp_projects.get_mut(&pid) {
                    p_mut.current_quantity =
                        p_mut.current_quantity.saturating_add(delivered_units as u64);
                    total_prog = p_mut.current_quantity as i64;
                    completed = p_mut.current_quantity >= p_mut.target_quantity;
                }
                if let Some(det) = snap.alliance_war_state.project_details.get_mut(&pid) {
                    let entry = det.char_contributions.entry(cid as i64).or_insert(0);
                    *entry = entry.saturating_add(delivered_units as u64);
                    char_total_prog = *entry;
                    total_contributors = det.char_contributions.len().max(1);
                    if completed {
                        det.status = "Completed".into();
                    }
                }
                if completed {
                    if let Some(m) = snap.alliance_war_state.project_meta.get_mut(&pid) {
                        m.2 = true;
                    }
                }
                corp_projects::sync_corp_project_contributors(snap, pid);
                true
            });
            if !applied {
                let _ = db.add_or_stack_item(
                    cid as i64,
                    consumed_loc,
                    eve_db::InventoryFlag::Hangar,
                    target_type_id,
                    delivered_units,
                );
                return Err((STATUS_BAD_REQUEST, "Project no longer active".into()));
            }
            let _ = db.add_or_stack_item(
                corpid as i64,
                consumed_loc,
                eve_db::InventoryFlag::CorpDeliveries,
                target_type_id,
                delivered_units,
            );
            state_deliver.refresh_from_db(now_u);

            let remainder = (target_prog_total - total_prog).max(0) as u64;
            ctx_queue_notice(
                NoticeScope::Character(cid),
                pb::message(
                    "eve_public.freelance.contributionmethod.itemdelivery.api.ContributionSucceededNotice",
                )
                .set("delivery.uuid", uuid.clone())
                .set("contributed", delivered_units as u64)
                .set("remainder", remainder)
                .must_to_any(),
                now_u,
            );

            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                pb::message("eve_public.freelance.project.api.ParticipantContributedNotice")
                    .set("project.uuid", uuid.clone())
                    .set("participant.sequential", cid)
                    .set("total_participant_progress", char_total_prog)
                    .must_to_any(),
                now_u,
            );

            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                pb::message("eve_public.freelance.project.api.ProgressedNotice")
                    .set("project.uuid", uuid.clone())
                    .set("total_project_progress", total_prog as u64)
                    .set("total_contributors", total_contributors as u32)
                    .must_to_any(),
                now_u,
            );

            if completed {
                ctx_queue_notice(
                    NoticeScope::Corporation(corpid),
                    pb::message("eve_public.freelance.project.api.CompletedNotice")
                        .set("project.uuid", uuid.clone())
                        .must_to_any(),
                    now_u,
                );
            }

            Ok(pb::message(
                "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverResponse",
            )
            .set("escrowed.delivery.uuid", uuid)
            .must_to_any())
        }),
    );

    // 20b. eve_public.freelance.contributionmethod.itemdelivery.api.GetStatusRequest -> GetStatusResponse
    let state_delivery_status = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.contributionmethod.itemdelivery.api.GetStatusRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uuid = extract_uuid(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing delivery UUID".into()))?;
            state_delivery_status.refresh_from_db(ctx_now_unix(ctx));
            let projects = state_delivery_status
                .projects
                .read()
                .unwrap_or_else(|e| e.into_inner());
            let p = projects
                .get(&uuid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Delivery project not found".into()))?;
            let rsp = pb::message(
                "eve_public.freelance.contributionmethod.itemdelivery.api.GetStatusResponse",
            );
            let rsp = if p.current_progress > 0 {
                rsp.set("succeeded.quantity_delivered", p.current_progress as u32)
            } else if p.state == 1 {
                rsp.set_with("escrowed", |v| v)
            } else {
                rsp.set_with("failed", |v| v)
            };
            Ok(rsp.must_to_any())
        }),
    );

    // 21. eve_public.freelance.contributionmethod.definition.api.GetAllLatestRequest
    {
        let sde = Arc::clone(&router.sde);
        router.register_handler(
            "eve_public.freelance.contributionmethod.definition.api.GetAllLatestRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                Ok(encode_freelance_contribution_methods(
                    &sde,
                    "eve_public.freelance.contributionmethod.definition.api.GetAllLatestResponse",
                    true,
                    1,
                ))
            }),
        );
    }

    // 22. eve_public.freelance.contributionmethod.definition.api.GetAllLatestWithinMajorRequest
    {
        let sde = Arc::clone(&router.sde);
        router.register_handler(
            "eve_public.freelance.contributionmethod.definition.api.GetAllLatestWithinMajorRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let major = dyn_msg
                    .get(1)
                    .and_then(|v| match v {
                        DynamicValue::Uint32(n) => Some(*n),
                        DynamicValue::Int32(n) => Some(*n as u32),
                        DynamicValue::Uint64(n) => Some(*n as u32),
                        DynamicValue::Int64(n) => Some(*n as u32),
                        _ => None,
                    })
                    .unwrap_or(1);
                let include_defs = major == 0 || major == 1;
                Ok(encode_freelance_contribution_methods(
                    &sde,
                    "eve_public.freelance.contributionmethod.definition.api.GetAllLatestWithinMajorResponse",
                    include_defs,
                    if major == 0 { 1 } else { major },
                ))
            }),
        );
    }
}

pub fn encode_freelance_contribution_methods(
    sde: &eve_sde::SdeRegistry,
    rsp_type_url: &str,
    include_defs: bool,
    major: u32,
) -> GoogleAny {
    let mut builder = pb::message(rsp_type_url)
        .set("version.major", major)
        .set("version.minor", 0u32)
        .set("version.patch", 0u32);

    if include_defs {
        if let Ok(methods) = sde.contribution_methods() {
            for (name, val) in &methods {
                builder = builder.add_with("definitions", |d| {
                    fill_single_freelance_def(d, name, val)
                });
            }
        }
    }

    builder.must_to_any()
}

fn set_loc_id(b: ProtoBuilder, field: &str, val: &serde_json::Value, key: &str) -> ProtoBuilder {
    let id = val
        .get(key)
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v > 0);
    match id {
        Some(seq) => b.set(&format!("{field}.sequential"), seq),
        None => b,
    }
}

fn set_name_id(b: ProtoBuilder, field: &str, name: &str) -> ProtoBuilder {
    b.when(!name.is_empty(), |b| b.set(&format!("{field}.name"), name))
}

fn fill_single_freelance_def(
    b: ProtoBuilder,
    name: &str,
    val: &serde_json::Value,
) -> ProtoBuilder {
    let tags: Vec<&str> = val
        .get("contentTags")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .collect();
    let params = val
        .get("parameters")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    b.set("id.name", name).set_with("attributes", |mut attr| {
        attr = set_loc_id(attr, "title", val, "titleID");
        attr = set_loc_id(attr, "description", val, "descriptionID");
        attr = set_loc_id(attr, "progress_description", val, "progressDescriptionID");
        attr = set_loc_id(attr, "target_description", val, "targetValueDescriptionID");
        attr = set_loc_id(attr, "reward_description", val, "rewardingDescriptionID");
        attr = set_name_id(
            attr,
            "icon",
            val.get("icon").and_then(|v| v.as_str()).unwrap_or(""),
        );
        for tag in tags {
            attr = attr.add_with("content_tags", |t| t.set("name", tag));
        }
        for p in &params {
            let key = p.get("parameterKey").and_then(|v| v.as_str()).unwrap_or("");
            let ptype = p
                .get("parameterType")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let picon = p.get("icon").and_then(|v| v.as_str()).unwrap_or("");
            let required = p.get("isRequired").and_then(|v| v.as_u64()).unwrap_or(0) != 0;
            let max_entries = p
                .get("maxEntries")
                .and_then(|v| v.as_u64())
                .and_then(|v| u32::try_from(v).ok());
            attr = attr.add_with("parameters", |pb_param| {
                pb_param.set_with("matcher", |m| {
                    let m = m
                        .set("key", key)
                        .set("type", ptype)
                        .set("optional", !required);
                    let m = set_name_id(m, "icon", picon);
                    match max_entries {
                        Some(max) => m.set("max_entries", max),
                        None => m,
                    }
                })
            });
        }
        attr
    })
}

#[cfg(test)]
fn encode_single_freelance_def(name: &str, val: &serde_json::Value, out: &mut Vec<u8>) {
    if let Err(err) = pb::message(
        "eve_public.freelance.contributionmethod.definition.api.GetAllLatestResponse",
    )
    .add_with("definitions", |d| fill_single_freelance_def(d, name, val))
    .encode_into(out)
    {
        panic!("encode_single_freelance_def failed: {err}");
    }
}

#[cfg(test)]
#[path = "quasar_freelance_defs_tests.rs"]
mod quasar_freelance_defs_tests;

#[cfg(test)]
#[path = "quasar_freelance_defs_edges_tests.rs"]
mod quasar_freelance_defs_edges_tests;

