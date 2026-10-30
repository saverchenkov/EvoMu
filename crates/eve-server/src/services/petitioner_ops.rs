//! `petitioner_ops` — petitioner service RPC handlers and data structures.
//!
//! Extracted from `account_services` to satisfy module size constraints (< 1,000 lines).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::account_services::{err, load_store, req_gm, try_with_store, with_store};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub fn default_petition_queue_id() -> i64 {
    1
}

// DERIVED: server-configured support petition categories (id, parent_id, name, description, is_billing) and queues.
pub const CANONICAL_PETITION_CATEGORIES: &[(i64, i64, &str, &str, bool)] = &[
    (1, 0, "Game Play Support", "General gameplay assistance", false),
    (2, 0, "Billing & Account", "Account and subscription support", true),
    (3, 0, "Technical Issues", "Client and connectivity issues", false),
    (4, 0, "Stuck Character", "Character or ship stuck in space", false),
    (101, 1, "Missions & Exploration", "Mission objectives and deadspace pockets", false),
    (102, 1, "Reimbursement", "Ship and module loss reimbursement requests", false),
    (201, 2, "Omega Subscription", "Subscription time and billing inquiries", true),
    (202, 2, "PLEX & NES", "PLEX vault and New Eden Store purchases", true),
    (301, 3, "Crash & Performance", "Client crash and graphics issues", false),
    (401, 4, "Stuck in Space", "Unable to warp, dock, or jump", false),
];

// DERIVED: server-configured GM support petition queues.
pub const CANONICAL_PETITION_QUEUES: &[(i64, &str)] = &[
    (1, "Default"),
    (2, "Escalated"),
    (3, "Billing"),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PetitionEntry {
    pub petition_id: i64,
    pub petitioner_cid: i64,
    pub subject: String,
    pub description: String,
    pub category_id: i64,
    #[serde(default = "default_petition_queue_id")]
    pub queue_id: i64,
    pub status: String,
    pub claimed_by: Option<i64>,
    pub escalated: bool,
    #[serde(default)]
    pub unread: bool,
    #[serde(default)]
    pub last_unread_message_id: i64,
    pub rating: Option<i64>,
    #[serde(default)]
    pub response_time_rating: Option<i64>,
    #[serde(default)]
    pub helpfulness_rating: Option<i64>,
    #[serde(default)]
    pub attitude_rating: Option<i64>,
    pub rating_comment: String,
    pub messages: Vec<(i64, String, u64)>,
    pub logs: Vec<(String, i64, u64)>,
}

/// Dispatches all `petitioner.*` RPC methods.
pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "petitioner" {
        return None;
    }
    match handle_petitioner(ctx, session, method, args, kwargs) {
        Ok(Some(rep)) => Some(Ok(rep)),
        Ok(None) => None,
        Err(e) => Some(Err(e)),
    }
}

fn handle_petitioner(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Result<Option<PyRep>, BridgeRpcError> {
    match method {
        "CreatePetition" => {
            let (_, cid) = ServerContext::require_character(session)?;
            let subject = args
                .first()
                .and_then(|a| a.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("");
            if subject.is_empty() {
                return Err(err(
                    "InvalidPetitionSubject",
                    "CreatePetition requires a non-empty subject",
                ));
            }
            let desc = args
                .get(1)
                .and_then(|a| a.as_str())
                .unwrap_or("Support request")
                .to_string();
            let cat_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(1);
            if !CANONICAL_PETITION_CATEGORIES
                .iter()
                .any(|&(id, _, _, _, _)| id == cat_id)
            {
                return Err(err(
                    "InvalidPetitionCategory",
                    format!("Unknown petition category {cat_id}"),
                ));
            }
            let now_unix = ctx.now_unix();
            let pet_id = with_store(ctx, |_, store| {
                let id = store.alloc_id();
                let mid = store.alloc_message_id();
                store.petitions.insert(
                    id,
                    PetitionEntry {
                        petition_id: id,
                        petitioner_cid: cid,
                        subject: subject.to_string(),
                        description: desc.clone(),
                        category_id: cat_id,
                        queue_id: 1,
                        status: "open".into(),
                        claimed_by: None,
                        escalated: false,
                        unread: false,
                        last_unread_message_id: mid,
                        rating: None,
                        response_time_rating: None,
                        helpfulness_rating: None,
                        attitude_rating: None,
                        rating_comment: String::new(),
                        messages: vec![(cid, desc, now_unix)],
                        logs: vec![("Created".into(), cid, now_unix)],
                    },
                );
                id
            });
            ctx.notify(
                NotificationTarget::Character(cid),
                "OnPetitionCreated",
                "charid",
                vec![PyRep::Integer(pet_id)],
            );
            ctx.notify(
                NotificationTarget::Character(cid),
                "OnNewPetition",
                "charid",
                vec![PyRep::Integer(pet_id)],
            );
            Ok(Some(PyRep::keyval([
                ("petitionID", PyRep::Integer(pet_id)),
                ("status", PyRep::String("open".into())),
                ("categoryID", PyRep::Integer(cat_id)),
            ])))
        }
        "GetMyPetitionsEx" => {
            let (_, cid) = ServerContext::require_character(session)?;
            let list = ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let mut rows: Vec<_> = store
                    .petitions
                    .values()
                    .filter(|p| p.petitioner_cid == cid)
                    .cloned()
                    .collect();
                rows.sort_by_key(|p| p.petition_id);
                rows.into_iter()
                    .map(|p| {
                        PyRep::keyval([
                            ("petitionID", PyRep::Integer(p.petition_id)),
                            ("subject", PyRep::String(p.subject)),
                            ("description", PyRep::String(p.description)),
                            ("categoryID", PyRep::Integer(p.category_id)),
                            ("status", PyRep::String(p.status)),
                            ("escalated", PyRep::Bool(p.escalated)),
                            (
                                "claimedBy",
                                p.claimed_by.map(PyRep::Integer).unwrap_or(PyRep::None),
                            ),
                            (
                                "rating",
                                p.rating.map(PyRep::Integer).unwrap_or(PyRep::None),
                            ),
                            (
                                "responseTimeRating",
                                p.response_time_rating
                                    .map(PyRep::Integer)
                                    .unwrap_or(PyRep::None),
                            ),
                            (
                                "helpfulnessRating",
                                p.helpfulness_rating
                                    .map(PyRep::Integer)
                                    .unwrap_or(PyRep::None),
                            ),
                            (
                                "attitudeRating",
                                p.attitude_rating
                                    .map(PyRep::Integer)
                                    .unwrap_or(PyRep::None),
                            ),
                        ])
                    })
                    .collect()
            });
            Ok(Some(PyRep::List(list)))
        }
        "HasOpenTickets" => {
            let (_, cid) = ServerContext::require_character(session)?;
            let has_open = ctx.db.with_snapshot(|snap| {
                load_store(snap)
                    .petitions
                    .values()
                    .any(|p| p.petitioner_cid == cid && p.status != "closed" && p.status != "cancelled")
            });
            Ok(Some(PyRep::Bool(has_open)))
        }
        "CancelPetition" => {
            let (_, cid) = ServerContext::require_character(session)?;
            let pet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let now_unix = ctx.now_unix();
            try_with_store(ctx, |_, store| {
                let Some(pet) = store.petitions.get_mut(&pet_id) else {
                    return Err(err("PetitionNotFound", format!("Petition {pet_id} not found")));
                };
                if pet.petitioner_cid != cid && !session.has_gm_role() {
                    return Err(err("InsufficientRole", "Cannot cancel another player's petition"));
                }
                if pet.status.eq_ignore_ascii_case("closed") || pet.status.eq_ignore_ascii_case("cancelled") {
                    return Err(err("PetitionClosed", "Cannot cancel a closed petition"));
                }
                pet.status = "cancelled".into();
                pet.logs.push(("Cancelled".into(), cid, now_unix));
                Ok(())
            })?;
            Ok(Some(PyRep::Bool(true)))
        }
        "DeletePetition" => {
            let (_, cid) = ServerContext::require_character(session)?;
            let pet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            try_with_store(ctx, |_, store| {
                let Some(pet) = store.petitions.get(&pet_id) else {
                    return Err(err("PetitionNotFound", format!("Petition {pet_id} not found")));
                };
                if pet.petitioner_cid != cid && !session.has_gm_role() {
                    return Err(err("InsufficientRole", "Cannot delete another player's petition"));
                }
                if !pet.status.eq_ignore_ascii_case("closed") && !pet.status.eq_ignore_ascii_case("cancelled") {
                    return Err(err("PetitionNotClosed", "Cannot delete an active petition"));
                }
                store.petitions.remove(&pet_id);
                Ok(())
            })?;
            Ok(Some(PyRep::Bool(true)))
        }
        "PetitionerChat" | "PetitioneeChat" => {
            let (_, cid) = ServerContext::require_character(session)?;
            if method == "PetitioneeChat" {
                req_gm(session, "PetitioneeChat")?;
            }
            let pet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let msg = args.get(1).and_then(|a| a.as_str()).unwrap_or("").to_string();
            let now_unix = ctx.now_unix();
            let (cnt, mid, petitioner_cid, claimed_by) = try_with_store(ctx, |_, store| {
                let is_gm_msg = {
                    let Some(pet) = store.petitions.get(&pet_id) else {
                        return Err(err("PetitionNotFound", format!("Petition {pet_id} not found")));
                    };
                    if pet.status.eq_ignore_ascii_case("closed") || pet.status.eq_ignore_ascii_case("cancelled") {
                        return Err(err("PetitionClosed", "Cannot post chat messages to a closed or cancelled petition"));
                    }
                    if method == "PetitionerChat" && pet.petitioner_cid != cid {
                        return Err(err("AccessDenied", "Only the petitioner may post player messages on this petition"));
                    }
                    method == "PetitioneeChat" || cid != pet.petitioner_cid
                };
                let mid = store.alloc_message_id();
                let pet = store.petitions.get_mut(&pet_id).expect("verified above");
                pet.messages.push((cid, msg.clone(), now_unix));
                if is_gm_msg {
                    pet.unread = true;
                    pet.last_unread_message_id = mid;
                    pet.status = "Responded".into();
                    pet.logs.push((format!("GMComment:{msg}"), cid, now_unix));
                } else {
                    pet.logs.push((format!("PlayerComment:{msg}"), cid, now_unix));
                }
                Ok((pet.messages.len(), mid, pet.petitioner_cid, pet.claimed_by))
            })?;
            let msg_args = vec![
                PyRep::Integer(pet_id),
                PyRep::Integer(cid),
                PyRep::String(msg),
                PyRep::Integer(mid),
            ];
            ctx.notify(NotificationTarget::Character(petitioner_cid), "OnPetitionMessage", "charid", msg_args.clone());
            if let Some(gm_cid) = claimed_by.filter(|&g| g != petitioner_cid) {
                ctx.notify(NotificationTarget::Character(gm_cid), "OnPetitionMessage", "charid", msg_args);
            }
            Ok(Some(PyRep::keyval([
                ("petitionID", PyRep::Integer(pet_id)),
                ("messageCount", PyRep::Integer(cnt as i64)),
            ])))
        }
        "GetPetitionQueue" | "GetClaimedPetitions" => {
            let (_, cid) = ServerContext::require_character(session)?;
            req_gm(session, method)?;
            let target_qid = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let list = ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let mut rows: Vec<_> = store
                    .petitions
                    .values()
                    .filter(|p| {
                        if method == "GetClaimedPetitions" {
                            p.claimed_by == Some(cid)
                        } else {
                            p.status != "closed" && p.status != "cancelled" && p.queue_id == target_qid
                        }
                    })
                    .collect();
                rows.sort_by_key(|p| p.petition_id);
                rows.into_iter()
                    .map(|p| {
                        PyRep::keyval([
                            ("petitionID", PyRep::Integer(p.petition_id)),
                            ("petitionerID", PyRep::Integer(p.petitioner_cid)),
                            ("subject", PyRep::String(p.subject.clone())),
                            ("status", PyRep::String(p.status.clone())),
                            ("escalated", PyRep::Bool(p.escalated)),
                            ("queueID", PyRep::Integer(p.queue_id)),
                            (
                                "claimedBy",
                                p.claimed_by.map(PyRep::Integer).unwrap_or(PyRep::None),
                            ),
                        ])
                    })
                    .collect()
            });
            Ok(Some(PyRep::List(list)))
        }
        "ClaimPetition" => {
            let (_, cid) = ServerContext::require_character(session)?;
            req_gm(session, "ClaimPetition")?;
            let pet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let now_unix = ctx.now_unix();
            let petitioner_cid = try_with_store(ctx, |_, store| {
                let Some(pet) = store.petitions.get_mut(&pet_id) else {
                    return Err(err("PetitionNotFound", format!("Petition {pet_id} not found")));
                };
                if pet.status.eq_ignore_ascii_case("closed") || pet.status.eq_ignore_ascii_case("cancelled") {
                    return Err(err("PetitionClosed", "Cannot claim a closed or cancelled petition"));
                }
                if pet.claimed_by.is_some_and(|c| c != cid) {
                    return Err(err("PetitionAlreadyClaimed", "Petition already claimed by another GM"));
                }
                pet.claimed_by = Some(cid);
                pet.status = "claimed".into();
                pet.logs.push(("Claimed".into(), cid, now_unix));
                Ok(pet.petitioner_cid)
            })?;
            let assign_args = vec![PyRep::Integer(pet_id), PyRep::Integer(cid)];
            ctx.notify(NotificationTarget::Character(petitioner_cid), "OnAssignPetition", "charid", assign_args.clone());
            if cid != petitioner_cid {
                ctx.notify(NotificationTarget::Character(cid), "OnAssignPetition", "charid", assign_args);
            }
            Ok(Some(PyRep::Integer(1)))
        }
        "UnClaimPetition" | "EscalatePetition" | "ClosePetition" => {
            let (_, cid) = ServerContext::require_character(session)?;
            req_gm(session, method)?;
            let now_unix = ctx.now_unix();
            let pet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let target_qid = args.get(1).and_then(|a| a.as_i64()).unwrap_or(2);
            let note = args
                .get(1)
                .and_then(|a| a.as_str())
                .or_else(|| args.get(2).and_then(|a| a.as_str()))
                .unwrap_or("");
            let (st, petitioner_cid, prev_claimed) = try_with_store(ctx, |_, store| {
                let Some(pet) = store.petitions.get(&pet_id) else {
                    return Err(err("PetitionNotFound", format!("Petition {pet_id} not found")));
                };
                // R161-P1-1: Reject terminal-state petitions.
                if pet.status.eq_ignore_ascii_case("closed") || pet.status.eq_ignore_ascii_case("cancelled") {
                    return Err(err("PetitionClosed", format!("Cannot {method} a closed or cancelled petition")));
                }
                if method == "UnClaimPetition" && pet.claimed_by.is_none() {
                    return Err(err("PetitionNotClaimed", "Petition is not currently claimed"));
                }
                if method == "EscalatePetition" && !CANONICAL_PETITION_QUEUES.iter().any(|&(qid, _)| qid == target_qid) {
                    return Err(err("InvalidPetitionQueue", format!("Unknown petition queue {target_qid}")));
                }
                let mid = if method == "ClosePetition" && !note.is_empty() {
                    Some(store.alloc_message_id())
                } else {
                    None
                };
                let pet = store.petitions.get_mut(&pet_id).expect("verified above");
                let prev_gm = pet.claimed_by;
                match method {
                    "UnClaimPetition" => {
                        pet.claimed_by = None;
                        pet.status = "open".into();
                        pet.logs.push(("Unclaimed".into(), cid, now_unix));
                    }
                    "EscalatePetition" => {
                        pet.queue_id = target_qid;
                        pet.claimed_by = None;
                        pet.escalated = true;
                        pet.status = "escalated".into();
                        pet.logs.push((
                            if note.is_empty() {
                                "Escalated".into()
                            } else {
                                format!("Escalated:{note}")
                            },
                            cid,
                            now_unix,
                        ));
                    }
                    "ClosePetition" => {
                        pet.status = "closed".into();
                        if let Some(m) = mid {
                            pet.messages.push((cid, note.to_string(), now_unix));
                            pet.unread = true;
                            pet.last_unread_message_id = m;
                        }
                        pet.logs.push((format!("Closed:{note}"), cid, now_unix));
                    }
                    _ => {}
                }
                Ok((pet.status.clone(), pet.petitioner_cid, prev_gm))
            })?;
            if method == "UnClaimPetition" {
                let unassign_args = vec![PyRep::Integer(pet_id), PyRep::None];
                ctx.notify(NotificationTarget::Character(petitioner_cid), "OnAssignPetition", "charid", unassign_args.clone());
                if cid != petitioner_cid {
                    ctx.notify(NotificationTarget::Character(cid), "OnAssignPetition", "charid", unassign_args);
                }
            } else if method == "ClosePetition" {
                let close_args = vec![PyRep::Integer(pet_id)];
                ctx.notify(NotificationTarget::Character(petitioner_cid), "OnClosePetition", "charid", close_args.clone());
                if cid != petitioner_cid {
                    ctx.notify(NotificationTarget::Character(cid), "OnClosePetition", "charid", close_args.clone());
                }
                if let Some(gm_cid) = prev_claimed.filter(|&g| g != petitioner_cid && g != cid) {
                    ctx.notify(NotificationTarget::Character(gm_cid), "OnClosePetition", "charid", close_args);
                }
            }
            Ok(Some(PyRep::keyval([
                ("petitionID", PyRep::Integer(pet_id)),
                ("status", PyRep::String(st)),
                ("gmID", PyRep::Integer(cid)),
            ])))
        }
        "GetPetitionMessages" | "GetLog" => {
            let (_, cid) = ServerContext::require_character(session)?;
            let pet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let Some(pet) = ctx
                .db
                .with_snapshot(|snap| load_store(snap).petitions.get(&pet_id).cloned())
            else {
                return Err(err("PetitionNotFound", format!("Petition {pet_id} not found")));
            };
            if pet.petitioner_cid != cid && !session.has_gm_role() {
                return Err(err(
                    "AccessDenied",
                    "Only the petitioner or a GM may read petition messages and logs",
                ));
            }
            if method == "GetPetitionMessages" {
                let msgs = pet
                    .messages
                    .into_iter()
                    .map(|(sender, text, ts)| {
                        PyRep::keyval([
                            ("senderID", PyRep::Integer(sender)),
                            ("text", PyRep::String(text)),
                            ("timestamp", PyRep::Integer(ts as i64)),
                        ])
                    })
                    .collect();
                Ok(Some(PyRep::List(msgs)))
            } else {
                // R161-P1-3: Resolve character names from DB snapshot before fallback.
                let logs = ctx.db.with_snapshot(|snap| {
                    pet.logs
                        .into_iter()
                        .map(|(ev, actor, ts)| {
                            let prefix = ev.split(':').next().unwrap_or("");
                            let eid = match prefix {
                                "Created" => 1,
                                "Claimed" => 2,
                                "Unclaimed" => 3,
                                "Escalated" => 4,
                                "Closed" => 5,
                                "GMComment" | "PlayerComment" => 6,
                                "Rated" => 7,
                                "Cancelled" => 8,
                                _ => 1,
                            };
                            let user_name = snap
                                .characters
                                .get(&actor)
                                .map(|c| c.name.clone())
                                .unwrap_or_else(|| format!("char_{actor}"));
                            PyRep::keyval([
                                ("event", PyRep::String(ev.clone())),
                                ("eventText", PyRep::String(ev)),
                                ("actorID", PyRep::Integer(actor)),
                                ("timestamp", PyRep::Integer(ts as i64)),
                                ("characterID", PyRep::Integer(actor)),
                                ("eventID", PyRep::Integer(eid)),
                                (
                                    "logDateTime",
                                    PyRep::Integer(
                                        ts as i64 * 10_000_000 + 116_444_736_000_000_000,
                                    ),
                                ),
                                ("userName", PyRep::String(user_name)),
                            ])
                        })
                        .collect()
                });
                Ok(Some(PyRep::List(logs)))
            }
        }
        "AddPetitionRating" | "UpdatePetitionRating" => {
            let (_, cid) = ServerContext::require_character(session)?;
            let pet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let resp_rating = args.get(1).and_then(|a| a.as_i64()).unwrap_or(10).clamp(0, 10);
            let help_rating = args.get(2).and_then(|a| a.as_i64()).unwrap_or(resp_rating).clamp(0, 10);
            let att_rating = args.get(3).and_then(|a| a.as_i64()).unwrap_or(resp_rating).clamp(0, 10);
            let comment = args
                .get(4)
                .and_then(|a| a.as_str())
                .or_else(|| args.get(2).and_then(|a| a.as_str()))
                .unwrap_or("")
                .to_string();
            let now_unix = ctx.now_unix();
            try_with_store(ctx, |_, store| {
                let Some(pet) = store.petitions.get_mut(&pet_id) else {
                    return Err(err("PetitionNotFound", format!("Petition {pet_id} not found")));
                };
                if pet.petitioner_cid != cid {
                    return Err(err(
                        "CharacterOwnershipMismatch",
                        "Only the petitioner can rate a petition",
                    ));
                }
                if pet.status != "closed" {
                    return Err(err("PetitionNotClosed", "Petition must be closed before rating"));
                }
                pet.rating = Some(resp_rating);
                pet.response_time_rating = Some(resp_rating);
                pet.helpfulness_rating = Some(help_rating);
                pet.attitude_rating = Some(att_rating);
                pet.rating_comment = comment.clone();
                pet.logs.push((format!("Rated:{resp_rating}"), cid, now_unix));
                Ok(())
            })?;
            Ok(Some(PyRep::keyval([
                ("petitionID", PyRep::Integer(pet_id)),
                ("rating", PyRep::Integer(resp_rating)),
                ("responseTimeRating", PyRep::Integer(resp_rating)),
                ("helpfulnessRating", PyRep::Integer(help_rating)),
                ("attitudeRating", PyRep::Integer(att_rating)),
                ("comment", PyRep::String(comment)),
            ])))
        }
        "GetCategories" => {
            let _ = ServerContext::require_character(session)?;
            let cats = CANONICAL_PETITION_CATEGORIES
                .iter()
                .map(|&(id, _, name, _, _)| {
                    PyRep::keyval([
                        ("categoryID", PyRep::Integer(id)),
                        ("categoryName", PyRep::String(name.into())),
                    ])
                })
                .collect();
            Ok(Some(PyRep::List(cats)))
        }
        "GetCategoryHierarchicalInfo" => {
            let _ = ServerContext::require_character(session)?;
            let mut parent_dict = Vec::new();
            let mut child_map: HashMap<i64, Vec<(PyRep, PyRep)>> = HashMap::new();
            let mut desc_dict = Vec::new();
            let mut billing_list = Vec::new();
            for &(id, pid, name, desc, is_billing) in CANONICAL_PETITION_CATEGORIES {
                let entry = PyRep::Tuple(vec![PyRep::String(name.into()), PyRep::String("EN".into())]);
                if pid == 0 {
                    parent_dict.push((PyRep::Integer(id), entry));
                    child_map.entry(id).or_default();
                } else {
                    child_map.entry(pid).or_default().push((PyRep::Integer(id), entry));
                }
                desc_dict.push((PyRep::Integer(id), PyRep::String(desc.into())));
                if is_billing {
                    billing_list.push(PyRep::Integer(id));
                }
            }
            let mut child_dict: Vec<(PyRep, PyRep)> = child_map
                .into_iter()
                .map(|(pid, children)| (PyRep::Integer(pid), PyRep::Dict(children)))
                .collect();
            child_dict.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));
            Ok(Some(PyRep::Tuple(vec![
                PyRep::Dict(parent_dict),
                PyRep::Dict(child_dict),
                PyRep::Dict(desc_dict),
                PyRep::List(billing_list),
            ])))
        }
        "GetCategoryProperties" => {
            let _ = ServerContext::require_character(session)?;
            let Some(cat_id) = args.first().and_then(|a| a.as_i64()) else {
                return Ok(Some(PyRep::List(Vec::new())));
            };
            let props = category_properties_for_id(cat_id);
            Ok(Some(PyRep::List(props)))
        }
        "GetClientPickerInfo" => {
            let (_, cid) = ServerContext::require_character(session)?;
            let lookup = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let items = ctx.db.with_snapshot(|snap| {
                let mut rows: Vec<(i64, String)> = snap
                    .items
                    .values()
                    .filter(|it| it.owner_id == cid)
                    .map(|it| {
                        let tname = ctx
                            .sde
                            .types
                            .get(&it.type_id)
                            .map(|t| t.name.clone())
                            .unwrap_or_else(|| format!("Item {}", it.type_id));
                        (it.item_id, tname)
                    })
                    .filter(|(_, name)| lookup.is_empty() || name.to_ascii_lowercase().contains(&lookup))
                    .collect();
                rows.sort_by_key(|(id, _)| *id);
                if rows.is_empty() {
                    if let Some(ch) = snap.characters.get(&cid) {
                        let sname = ctx
                            .sde
                            .solar_systems
                            .get(&ch.solar_system_id)
                            .map(|s| s.name.clone())
                            .unwrap_or_else(|| format!("System {}", ch.solar_system_id));
                        rows.push((i64::from(ch.solar_system_id), sname));
                    }
                }
                rows.into_iter()
                    .map(|(id, label)| PyRep::Tuple(vec![PyRep::String(label), PyRep::Integer(id)]))
                    .collect()
            });
            Ok(Some(PyRep::List(items)))
        }
        "GetQueues" => {
            let _ = ServerContext::require_character(session)?;
            req_gm(session, "GetQueues")?;
            let queues = CANONICAL_PETITION_QUEUES
                .iter()
                .map(|&(id, name)| {
                    PyRep::keyval([
                        ("queueID", PyRep::Integer(id)),
                        ("queueName", PyRep::String(name.into())),
                    ])
                })
                .collect();
            Ok(Some(PyRep::List(queues)))
        }
        "GetUnreadMessages" => {
            let (_, cid) = ServerContext::require_character(session)?;
            let unread = ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let mut rows: Vec<_> = store
                    .petitions
                    .values()
                    .filter(|p| p.petitioner_cid == cid && p.unread)
                    .collect();
                rows.sort_by_key(|p| p.petition_id);
                rows.into_iter()
                    .map(|p| {
                        let last_text = p
                            .messages
                            .iter()
                            .rev()
                            .find(|(s, _, _)| *s != p.petitioner_cid)
                            .or_else(|| p.messages.last())
                            .map(|(_, t, _)| t.clone())
                            .unwrap_or_else(|| p.description.clone());
                        let mid = if p.last_unread_message_id > 0 {
                            p.last_unread_message_id
                        } else {
                            p.messages.len() as i64
                        };
                        PyRep::keyval([
                            ("petitionID", PyRep::Integer(p.petition_id)),
                            ("text", PyRep::String(last_text)),
                            ("messageID", PyRep::Integer(mid)),
                        ])
                    })
                    .collect()
            });
            Ok(Some(PyRep::List(unread)))
        }
        "GetUserCatalogCountry" => {
            let (uid, _) = ServerContext::require_character(session)?;
            // DERIVED: default catalog country code "US" when account country is unset in store.
            let country = ctx.db.with_snapshot(|snap| {
                load_store(snap)
                    .user_countries
                    .get(&uid)
                    .cloned()
                    .unwrap_or_else(|| "US".into())
            });
            Ok(Some(PyRep::String(country)))
        }
        "GetZendeskJwtLink" => {
            let (uid, cid) = ServerContext::require_character(session)?;
            let return_to = args.first().and_then(|a| a.as_str()).unwrap_or("hc/en-us");
            // DERIVED: deterministic Zendesk SSO redirect URL with character/account identity when Zendesk support is enabled.
            let link = ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                if store.zendesk_disabled {
                    PyRep::None
                } else {
                    let cname = snap
                        .characters
                        .get(&cid)
                        .map(|c| c.name.replace(' ', "_"))
                        .unwrap_or_else(|| format!("char_{cid}"));
                    PyRep::String(format!(
                        "https://support.eveonline.com/access/jwt?jwt=eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.uid_{uid}_cid_{cid}_{cname}&return_to={return_to}"
                    ))
                }
            });
            Ok(Some(link))
        }
        "IsSerenity" => {
            // const-ok: returns false on Tranquility and Western clusters
            let _ = ServerContext::require_character(session)?;
            Ok(Some(PyRep::Bool(false)))
        }
        "IsZendeskEnabled" => {
            let _ = ServerContext::require_character(session)?;
            let enabled = ctx.db.with_snapshot(|snap| !load_store(snap).zendesk_disabled);
            Ok(Some(PyRep::Bool(enabled)))
        }
        "IsZendeskSwapEnabled" => {
            // const-ok: Zendesk swap migration flag is disabled
            let _ = ServerContext::require_character(session)?;
            Ok(Some(PyRep::Bool(false)))
        }
        "MarkAsRead" => {
            let (_, cid) = ServerContext::require_character(session)?;
            let pid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            try_with_store(ctx, |_, store| {
                let Some(p) = store.petitions.get_mut(&pid) else {
                    return Err(err("PetitionNotFound", format!("Petition {pid} not found")));
                };
                if p.petitioner_cid != cid && !session.has_gm_role() {
                    return Err(err("AccessDenied", "Cannot mark another player's petition as read"));
                }
                p.unread = false;
                // R161-P1-2: When status was Responded, restore claimed if claimed_by is some, otherwise open.
                if p.status.eq_ignore_ascii_case("Responded") {
                    p.status = if p.claimed_by.is_some() {
                        "claimed".into()
                    } else {
                        "open".into()
                    };
                }
                Ok(())
            })?;
            Ok(Some(PyRep::Bool(true)))
        }
        "MayPetition" => {
            let cid = match session.character_id {
                Some(id) if id > 0 => id,
                _ => return Ok(Some(PyRep::Integer(-6))),
            };
            if ctx.db.get_character(cid).is_err() {
                return Ok(Some(PyRep::Integer(-6)));
            }

            let cat_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let sub_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);

            if cat_id > 0 && !CANONICAL_PETITION_CATEGORIES.iter().any(|&(id, _, _, _, _)| id == cat_id) {
                return Ok(Some(PyRep::Integer(-4)));
            }

            let (is_banned, is_disabled, open_count, is_cooldown) = with_store(ctx, |snap, store| {
                let banned = if store.underage_users.contains(&(cid as u64)) {
                    true
                } else {
                    let uid = session.user_id.or_else(|| snap.characters.get(&cid).map(|c| c.user_id));
                    uid.and_then(|u| snap.accounts.values().find(|a| a.user_id == u))
                        .map_or(false, |acc| {
                            acc.check_active_restriction(ctx.now_unix()).is_err()
                                || acc.banned_until_unix.map_or(false, |until| ctx.now_unix() < until)
                        })
                };

                let disabled = store.zendesk_disabled
                    || store.disabled_categories.contains(&cat_id)
                    || (sub_id > 0 && store.disabled_categories.contains(&sub_id));

                let mut open = 0;
                let mut latest_created_at: Option<u64> = None;
                for p in store.petitions.values() {
                    if p.petitioner_cid == cid {
                        if !p.status.eq_ignore_ascii_case("closed") && !p.status.eq_ignore_ascii_case("cancelled") {
                            open += 1;
                        }
                        if let Some(&(_, _, ts)) = p.messages.first() {
                            latest_created_at = Some(latest_created_at.map_or(ts, |prev| prev.max(ts)));
                        }
                    }
                }
                let cooldown = latest_created_at.map_or(false, |ts| ctx.now_unix() < ts.saturating_add(60));

                (banned, disabled, open, cooldown)
            });

            if is_banned {
                return Ok(Some(PyRep::Integer(-1)));
            }
            if is_disabled {
                return Ok(Some(PyRep::Integer(-3)));
            }
            if open_count >= 10 {
                return Ok(Some(PyRep::Integer(-2)));
            }
            if is_cooldown {
                return Ok(Some(PyRep::Integer(-7)));
            }
            Ok(Some(PyRep::Integer(1)))
        }
        "PropertyPopulationInfo" => {
            let (_, cid) = ServerContext::require_character(session)?;
            let prop_str = args
                .first()
                .and_then(|a| a.as_str())
                .or_else(|| args.get(1).and_then(|a| a.as_str()));
            let prop_id = if prop_str.is_none() {
                args.get(1)
                    .and_then(|a| a.as_i64())
                    .or_else(|| args.first().and_then(|a| a.as_i64()))
            } else {
                None
            };
            let options = ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&cid) else {
                    return Vec::new();
                };
                if prop_str.is_some_and(|s| {
                    s.eq_ignore_ascii_case("SolarSystem") || s.eq_ignore_ascii_case("location")
                }) || prop_id == Some(2)
                {
                    let sname = ctx
                        .sde
                        .solar_systems
                        .get(&ch.solar_system_id)
                        .map(|s| s.name.clone())
                        .unwrap_or_else(|| format!("System {}", ch.solar_system_id));
                    return vec![PyRep::Tuple(vec![
                        PyRep::String(sname),
                        PyRep::Integer(i64::from(ch.solar_system_id)),
                    ])];
                }
                if prop_str.is_some_and(|s| s.eq_ignore_ascii_case("Character")) || prop_id == Some(4) {
                    let mut chars: Vec<_> = snap
                        .characters
                        .values()
                        .filter(|c| c.user_id == ch.user_id)
                        .map(|c| (c.character_id, c.name.clone()))
                        .collect();
                    chars.sort_by_key(|(id, _)| *id);
                    return chars
                        .into_iter()
                        .map(|(id, n)| PyRep::Tuple(vec![PyRep::String(n), PyRep::Integer(id)]))
                        .collect();
                }
                let mut ships: Vec<(i64, String)> = snap
                    .items
                    .values()
                    .filter(|it| {
                        it.owner_id == cid
                            && (it.item_id == ch.active_ship_id
                                || ctx
                                    .sde
                                    .types
                                    .get(&it.type_id)
                                    .is_some_and(|t| t.category_id == 6))
                    })
                    .map(|it| {
                        let tname = ctx
                            .sde
                            .types
                            .get(&it.type_id)
                            .map(|t| t.name.clone())
                            .unwrap_or_else(|| format!("Ship {}", it.type_id));
                        (it.item_id, tname)
                    })
                    .collect();
                ships.sort_by_key(|(id, _)| *id);
                ships
                    .into_iter()
                    .map(|(id, label)| PyRep::Tuple(vec![PyRep::String(label), PyRep::Integer(id)]))
                    .collect()
            });
            Ok(Some(PyRep::List(options)))
        }
        _ => Ok(None),
    }
}

pub fn category_properties_for_id(cat_id: i64) -> Vec<PyRep> {
    let mut props = Vec::new();
    match cat_id {
        4 | 401 => {
            // Stuck Character / Stuck in Space
            props.push(PyRep::keyval([
                ("propertyName", PyRep::String("SolarSystem".into())),
                ("propertyDescriptionID", PyRep::Integer(1001)),
                ("propertyInputType", PyRep::Integer(2)),
                ("propertyInputInfo", PyRep::String("SolarSystem".into())),
                ("propertyRequiredInt", PyRep::Integer(1)),
                ("propertyID", PyRep::Integer(2)),
            ]));
            props.push(PyRep::keyval([
                ("propertyName", PyRep::String("Character".into())),
                ("propertyDescriptionID", PyRep::Integer(1002)),
                ("propertyInputType", PyRep::Integer(2)),
                ("propertyInputInfo", PyRep::String("Character".into())),
                ("propertyRequiredInt", PyRep::Integer(1)),
                ("propertyID", PyRep::Integer(4)),
            ]));
        }
        102 => {
            // Reimbursement
            props.push(PyRep::keyval([
                ("propertyName", PyRep::String("ShipType".into())),
                ("propertyDescriptionID", PyRep::Integer(1003)),
                ("propertyInputType", PyRep::Integer(2)),
                ("propertyInputInfo", PyRep::String("ShipType".into())),
                ("propertyRequiredInt", PyRep::Integer(1)),
                ("propertyID", PyRep::Integer(3)),
            ]));
            props.push(PyRep::keyval([
                ("propertyName", PyRep::String("SolarSystem".into())),
                ("propertyDescriptionID", PyRep::Integer(1001)),
                ("propertyInputType", PyRep::Integer(2)),
                ("propertyInputInfo", PyRep::String("SolarSystem".into())),
                ("propertyRequiredInt", PyRep::Integer(1)),
                ("propertyID", PyRep::Integer(2)),
            ]));
        }
        101 => {
            // Missions & Exploration
            props.push(PyRep::keyval([
                ("propertyName", PyRep::String("SolarSystem".into())),
                ("propertyDescriptionID", PyRep::Integer(1001)),
                ("propertyInputType", PyRep::Integer(2)),
                ("propertyInputInfo", PyRep::String("SolarSystem".into())),
                ("propertyRequiredInt", PyRep::Integer(1)),
                ("propertyID", PyRep::Integer(2)),
            ]));
        }
        2 | 201 | 202 => {
            // Billing & Account
            props.push(PyRep::keyval([
                ("propertyName", PyRep::String("Character".into())),
                ("propertyDescriptionID", PyRep::Integer(1002)),
                ("propertyInputType", PyRep::Integer(2)),
                ("propertyInputInfo", PyRep::String("Character".into())),
                ("propertyRequiredInt", PyRep::Integer(0)),
                ("propertyID", PyRep::Integer(4)),
            ]));
        }
        _ => {
            props.push(PyRep::keyval([
                ("propertyName", PyRep::String("SolarSystem".into())),
                ("propertyDescriptionID", PyRep::Integer(1001)),
                ("propertyInputType", PyRep::Integer(2)),
                ("propertyInputInfo", PyRep::String("SolarSystem".into())),
                ("propertyRequiredInt", PyRep::Integer(0)),
                ("propertyID", PyRep::Integer(2)),
            ]));
            props.push(PyRep::keyval([
                ("propertyName", PyRep::String("Character".into())),
                ("propertyDescriptionID", PyRep::Integer(1002)),
                ("propertyInputType", PyRep::Integer(2)),
                ("propertyInputInfo", PyRep::String("Character".into())),
                ("propertyRequiredInt", PyRep::Integer(1)),
                ("propertyID", PyRep::Integer(4)),
            ]));
        }
    }
    props
}

