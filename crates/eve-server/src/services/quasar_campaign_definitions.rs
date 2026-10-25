//! `quasar_campaign_definitions` — V24.01 Quasar Campaigns, Objectives, Tracks & Definitions
//! (`feat:quasar.campaign`, `quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/api/requests_pb2.pyj`,
//!  `quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/api/requests_pb2.pyj`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_PRECONDITION_FAILED,
};
use super::quasar_progression_extra::{CAMPAIGN_REWARD_ISK_PER_UNIT, ProgressionExtraManager};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicValue, GoogleAny,
    TYPE_GOOGLEAPIS_PREFIX,
};
use std::sync::Arc;

pub fn extract_campaign_id(val: Option<&DynamicValue>) -> Option<u64> {
    fn from_bytes(b: &[u8]) -> Option<u64> {
        if b.is_empty() || b.iter().all(|&x| x == 0) {
            return None;
        }
        if b.len() == 16 {
            let mut hi = [0u8; 8];
            let mut lo = [0u8; 8];
            hi.copy_from_slice(&b[0..8]);
            lo.copy_from_slice(&b[8..16]);
            if hi == [0u8; 8] {
                return Some(u64::from_be_bytes(lo)).filter(|&v| v != 0);
            }
            if lo == [0u8; 8] {
                return Some(u64::from_le_bytes(hi)).filter(|&v| v != 0);
            }
            return Some(u64::from_be_bytes(lo)).filter(|&v| v != 0);
        }
        if b.len() >= 8 {
            let mut arr = [0u8; 8];
            arr.copy_from_slice(&b[0..8]);
            let id = u64::from_be_bytes(arr);
            if id != 0 {
                return Some(id);
            }
        }
        if b.len() >= 4 {
            let mut arr = [0u8; 4];
            arr.copy_from_slice(&b[0..4]);
            let id = u32::from_be_bytes(arr) as u64;
            if id != 0 {
                return Some(id);
            }
        }
        b.first().copied().map(u64::from).filter(|&v| v != 0)
    }

    match val {
        Some(DynamicValue::Uint64(u)) if *u != 0 => Some(*u),
        Some(DynamicValue::Int64(i)) if *i > 0 => Some(*i as u64),
        Some(DynamicValue::Uint32(u)) if *u != 0 => Some(*u as u64),
        Some(DynamicValue::Int32(i)) if *i > 0 => Some(*i as u64),
        Some(DynamicValue::Bytes(b)) => from_bytes(b),
        Some(DynamicValue::Message(m)) => match m.get(1) {
            Some(DynamicValue::Uint64(u)) if *u != 0 => Some(*u),
            Some(DynamicValue::Int64(i)) if *i > 0 => Some(*i as u64),
            Some(DynamicValue::Uint32(u)) if *u != 0 => Some(*u as u64),
            Some(DynamicValue::Int32(i)) if *i > 0 => Some(*i as u64),
            Some(DynamicValue::Bytes(b)) => from_bytes(b),
            Some(DynamicValue::Message(inner)) => {
                extract_campaign_id(Some(&DynamicValue::Message(inner.clone())))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Fallback definition specs for authentic EVE Online contribution methods.
pub const AUTHENTIC_CONTRIBUTION_METHODS: &[(&str, u32, u32, &str, &[(&str, &str, &str)])] = &[
    (
        "ManufactureItem",
        665394,
        696246,
        "production",
        &[
            ("item_type", "manufacturable_item_type", "inventory"),
            ("owner", "on_behalf_of", "user"),
            ("facility_location", "corporation_office_location", "location"),
        ],
    ),
    (
        "CaptureFWComplex",
        665425,
        696248,
        "attack_fw_complex",
        &[
            ("faction", "empire_faction", "corporation_management"),
            ("solar_system", "location", "location"),
            ("complex_type", "factional_warfare_complex_type", "fw_complex_type"),
        ],
    ),
    (
        "DamageShip",
        659201,
        696243,
        "damage",
        &[
            ("solar_system", "location", "location"),
            ("ship", "ship_type", "spaceship_command"),
            ("organization", "identity", "pilot_or_organization"),
        ],
    ),
    (
        "DestroyShip",
        659203,
        696244,
        "combat",
        &[
            ("solar_system", "location", "location"),
            ("target_ship_type", "ship_type", "spaceship_command"),
        ],
    ),
    (
        "MineAsteroid",
        665390,
        696245,
        "mining",
        &[
            ("ore_type", "harvestable_type", "mining"),
            ("solar_system", "location", "location"),
        ],
    ),
    (
        "MineIce",
        665391,
        696247,
        "mining",
        &[
            ("ore_type", "harvestable_type", "mining"),
            ("solar_system", "location", "location"),
        ],
    ),
    (
        "MineGas",
        665392,
        696249,
        "mining",
        &[
            ("ore_type", "harvestable_type", "mining"),
            ("solar_system", "location", "location"),
        ],
    ),
    (
        "MineMoon",
        665393,
        696250,
        "mining",
        &[
            ("ore_type", "harvestable_type", "mining"),
            ("solar_system", "location", "location"),
        ],
    ),
    (
        "ScanSignature",
        665400,
        696251,
        "exploration",
        &[
            ("signature_group", "cosmic_signature_type", "scanning"),
            ("solar_system", "location", "location"),
        ],
    ),
    (
        "DeliverItem",
        665410,
        696252,
        "logistics",
        &[
            ("item_type", "item_type", "inventory"),
            ("destination", "structure_or_station", "location"),
        ],
    ),
    (
        "SalvageWreck",
        665415,
        696253,
        "salvaging",
        &[
            ("wreck_type", "wreck_type", "salvaging"),
            ("solar_system", "location", "location"),
        ],
    ),
    (
        "HackSite",
        665420,
        696254,
        "hacking",
        &[
            ("site_type", "exploration_site_type", "hacking"),
            ("solar_system", "location", "location"),
        ],
    ),
    (
        "CompleteMission",
        665430,
        696255,
        "missions",
        &[
            ("agent", "agent_id", "character"),
            ("mission_type", "mission_type", "missions"),
        ],
    ),
    (
        "EarnISK",
        665435,
        696256,
        "wallet",
        &[("source", "income_source", "wallet")],
    ),
    (
        "SpendISK",
        665440,
        696257,
        "wallet",
        &[("sink", "expense_type", "wallet")],
    ),
    (
        "EarnLoyaltyPoints",
        665445,
        696258,
        "loyalty_points",
        &[("corporation", "npc_corporation", "corporation_management")],
    ),
    (
        "EarnBounty",
        665450,
        696259,
        "bounty",
        &[("solar_system", "location", "location")],
    ),
    (
        "DestroyNPC",
        665455,
        696260,
        "combat",
        &[
            ("faction", "npc_faction", "corporation_management"),
            ("solar_system", "location", "location"),
        ],
    ),
    (
        "KillFactionalWarfareNPC",
        665460,
        696261,
        "attack_fw_complex",
        &[
            ("faction", "empire_faction", "corporation_management"),
            ("solar_system", "location", "location"),
        ],
    ),
    (
        "JumpStargate",
        665465,
        696262,
        "navigation",
        &[("origin", "location", "location")],
    ),
    (
        "RepairShip",
        665470,
        696263,
        "remote_repair",
        &[("repair_type", "repair_type", "fleet")],
    ),
    (
        "LootWreck",
        665475,
        696264,
        "cargo",
        &[("solar_system", "location", "location")],
    ),
];

pub const AUTHENTIC_TRIGGER_EVENTMATCHES: &[(&str, &[(&str, &str)])] = &[
    ("FWComplexCaptured", &[("complex_type", "fw"), ("system", "loc")]),
    ("ShipDamaged", &[("damage_amount", "qty"), ("target_type", "vessel")]),
    ("ShipDestroyed", &[("victim_ship", "vessel"), ("system", "loc")]),
    ("NPCDestroyed", &[("npc_type", "target"), ("bounty", "isk")]),
    ("ItemManufactured", &[("blueprint", "mfg_type"), ("runs", "qty")]),
    ("OreMined", &[("ore_type", "harvest"), ("units", "volume")]),
    ("SignatureScanned", &[("sig_group", "sig"), ("difficulty", "diff")]),
    ("MissionCompleted", &[("mission_id", "id"), ("agent_id", "agent")]),
    ("ItemDelivered", &[("item_type", "haul"), ("quantity", "qty")]),
    ("BountyEarned", &[("bounty_amount", "isk"), ("system", "loc")]),
];

/// Encodes a contribution method definition into protobuf wire format.
pub fn encode_contribution_method_definition(
    name: &str,
    title_id: u32,
    progress_desc_id: u32,
    icon: &str,
    params: &[(&str, &str, &str)],
    out: &mut Vec<u8>,
) {
    let mut id_bytes = Vec::new();
    encode_string_field(1, name, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, out);

    let mut attr_bytes = Vec::new();
    for (p_key, p_type, p_icon) in params {
        let mut p_bytes = Vec::new();
        let mut m_bytes = Vec::new();
        encode_string_field(1, p_key, &mut m_bytes);
        encode_string_field(2, p_type, &mut m_bytes);
        if !p_icon.is_empty() {
            let mut pi_bytes = Vec::new();
            encode_string_field(1, p_icon, &mut pi_bytes);
            encode_bytes_field(6, &pi_bytes, &mut m_bytes);
        }
        encode_bytes_field(1, &m_bytes, &mut p_bytes);
        encode_bytes_field(1, &p_bytes, &mut attr_bytes);
    }
    if title_id > 0 {
        let mut t_bytes = Vec::new();
        encode_varint_field(1, title_id as u64, &mut t_bytes);
        encode_bytes_field(2, &t_bytes, &mut attr_bytes);
    }
    if progress_desc_id > 0 {
        let mut pd_bytes = Vec::new();
        encode_varint_field(1, progress_desc_id as u64, &mut pd_bytes);
        encode_bytes_field(3, &pd_bytes, &mut attr_bytes);
    }
    if !icon.is_empty() {
        let mut ic_bytes = Vec::new();
        encode_string_field(1, icon, &mut ic_bytes);
        encode_bytes_field(4, &ic_bytes, &mut attr_bytes);
    }
    encode_bytes_field(2, &attr_bytes, out);
}

/// Encodes a trigger eventmatch definition into protobuf wire format.
pub fn encode_trigger_eventmatch_definition(
    name: &str,
    params: &[(&str, &str)],
    out: &mut Vec<u8>,
) {
    let mut id_bytes = Vec::new();
    encode_string_field(1, name, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, out);

    let mut attr_bytes = Vec::new();
    for (p_key, _p_ui) in params {
        let mut p_bytes = Vec::new();
        encode_string_field(1, p_key, &mut p_bytes);
        encode_bytes_field(1, &p_bytes, &mut attr_bytes);
    }
    encode_bytes_field(2, &attr_bytes, out);
}

/// Registers all campaign and campaign definition handlers on the public router.
pub fn register_campaign_definition_handlers(
    router: &PublicGatewayRouter,
    manager: Arc<ProgressionExtraManager>,
) {
    // 1. eve_public.campaign.api.GetAllRequest
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.api.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let camps = mgr.campaigns.read().unwrap_or_else(|e| e.into_inner());
            let mut out = Vec::new();
            for c in camps.values().filter(|c| c.active) {
                let mut id_bytes = Vec::new();
                encode_varint_field(1, c.id, &mut id_bytes);
                encode_bytes_field(1, &id_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.api.GetAllResponse"),
                value: out,
            })
        }),
    );

    // 2. eve_public.campaign.api.GetRequest
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.api.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let cid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign ID".into()))?;
            let camps = mgr.campaigns.read().unwrap_or_else(|e| e.into_inner());
            let camp = camps.get(&cid).ok_or((STATUS_NOT_FOUND, "Campaign not found".into()))?;
            let mut out = Vec::new();
            let mut attr_bytes = Vec::new();
            encode_string_field(1, &camp.name, &mut attr_bytes);
            encode_string_field(2, &camp.description, &mut attr_bytes);
            encode_bytes_field(1, &attr_bytes, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.api.GetResponse"),
                value: out,
            })
        }),
    );

    // 3. eve_public.campaign.contributionmethod.definition.api.GetAllRequest
    let mgr_cm = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.contributionmethod.definition.api.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let mut out = Vec::new();
            let mut encoded_any = false;

            if let Some(ref sde) = mgr_cm.sde {
                if let Ok(methods) = sde.contribution_methods() {
                    for (name, val) in &methods {
                        let title_id = val.get("titleID").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        let progress_desc_id = val.get("progressDescriptionID").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        let icon = val.get("icon").and_then(|v| v.as_str()).unwrap_or("");
                        let mut param_list = Vec::new();
                        if let Some(params_arr) = val.get("parameters").and_then(|v| v.as_array()) {
                            for p in params_arr {
                                let key = p.get("parameterKey").and_then(|v| v.as_str()).unwrap_or("");
                                let ptype = p.get("parameterType").and_then(|v| v.as_str()).unwrap_or("");
                                let picon = p.get("icon").and_then(|v| v.as_str()).unwrap_or("");
                                param_list.push((key, ptype, picon));
                            }
                        }
                        let mut def_bytes = Vec::new();
                        encode_contribution_method_definition(name, title_id, progress_desc_id, icon, &param_list, &mut def_bytes);
                        encode_bytes_field(1, &def_bytes, &mut out);
                    }
                    encoded_any = !methods.is_empty();
                }
            }

            if !encoded_any {
                for (name, title_id, prog_desc, icon, params) in AUTHENTIC_CONTRIBUTION_METHODS {
                    let mut def_bytes = Vec::new();
                    encode_contribution_method_definition(name, *title_id, *prog_desc, icon, params, &mut def_bytes);
                    encode_bytes_field(1, &def_bytes, &mut out);
                }
            }

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.contributionmethod.definition.api.GetAllResponse"),
                value: out,
            })
        }),
    );

    // 4. eve_public.campaign.objective.api.GetRequest
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let oid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign objective ID".into()))?;
            let objs = mgr.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let obj = objs.get(&oid).ok_or((STATUS_NOT_FOUND, "Campaign objective not found".into()))?;
            let mut out = Vec::new();
            let mut attr = Vec::new();
            encode_string_field(1, &obj.name, &mut attr);
            encode_varint_field(2, obj.target_progress as u64, &mut attr);
            encode_varint_field(3, obj.current_progress as u64, &mut attr);
            encode_bytes_field(1, &attr, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.objective.api.GetResponse"),
                value: out,
            })
        }),
    );

    // 5. eve_public.campaign.objective.api.GetStatusRequest
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.GetStatusRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let oid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign objective ID".into()))?;
            let objs = mgr.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let obj = objs.get(&oid).ok_or((STATUS_NOT_FOUND, "Campaign objective not found".into()))?;
            let mut out = Vec::new();
            encode_varint_field(1, obj.current_progress as u64, &mut out);
            encode_varint_field(2, obj.target_progress as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.objective.api.GetStatusResponse"),
                value: out,
            })
        }),
    );

    // 6. eve_public.campaign.objective.api.GetParticipantStatusRequest
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.GetParticipantStatusRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let oid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign objective ID".into()))?;
            let objs = mgr.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let obj = objs.get(&oid).ok_or((STATUS_NOT_FOUND, "Campaign objective not found".into()))?;
            let committed = obj.committed_participants.contains(&cid);
            let (prog, unredeemed) = obj.contributors.get(&cid).copied().unwrap_or((0, 0));
            let mut out = Vec::new();
            encode_varint_field(1, if committed { 1 } else { 0 }, &mut out);
            encode_varint_field(2, prog as u64, &mut out);
            encode_varint_field(3, unredeemed as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.objective.api.GetParticipantStatusResponse"),
                value: out,
            })
        }),
    );

    // 7. eve_public.campaign.objective.api.CommitRequest
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.CommitRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let oid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign objective ID".into()))?;
            {
                let mut objs = mgr.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
                let obj = objs.get_mut(&oid).ok_or((STATUS_NOT_FOUND, "Campaign objective not found".into()))?;
                if !obj.committed_participants.contains(&cid) {
                    obj.committed_participants.push(cid);
                }
            }
            mgr.save_to_db();
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.objective.api.CommitResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 8. eve_public.campaign.objective.api.ResignRequest
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.ResignRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let oid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign objective ID".into()))?;
            {
                let mut objs = mgr.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
                let obj = objs.get_mut(&oid).ok_or((STATUS_NOT_FOUND, "Campaign objective not found".into()))?;
                obj.committed_participants.retain(|c| *c != cid);
            }
            mgr.save_to_db();
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.objective.api.ResignResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 9. eve_public.campaign.objective.api.RedeemRewardsRequest
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.RedeemRewardsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let oid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign objective ID".into()))?;
            let (redeemable_units, payout) = {
                let mut objs = mgr.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
                let obj = objs.get_mut(&oid).ok_or((STATUS_NOT_FOUND, "Campaign objective not found".into()))?;
                let pool = obj.reward_pool_isk;
                let unredeemed = obj.contributors.get(&cid).map(|e| e.1).unwrap_or(0);
                let redeemable_units = unredeemed.min(pool / CAMPAIGN_REWARD_ISK_PER_UNIT);
                if unredeemed <= 0 || redeemable_units <= 0 {
                    return Err((STATUS_PRECONDITION_FAILED, "Campaign reward pool is empty or no rewards available".into()));
                }
                let payout = redeemable_units.saturating_mul(CAMPAIGN_REWARD_ISK_PER_UNIT);
                if let Some(entry) = obj.contributors.get_mut(&cid) {
                    entry.1 = entry.1.saturating_sub(redeemable_units);
                }
                obj.reward_pool_isk = obj.reward_pool_isk.saturating_sub(payout);
                (redeemable_units, payout)
            };
            let Some(ref db) = mgr.db else {
                return Err((STATUS_PRECONDITION_FAILED, "Database unavailable for reward payout".into()));
            };
            let reward_isk = eve_db::IskCents::from_isk(payout);
            let mut credited = false;
            db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&(cid as i64)) {
                    if let Some(new_bal) = ch.balance.checked_add(reward_isk) {
                        ch.balance = new_bal;
                        credited = true;
                        snap.append_journal(
                            cid as i64,
                            "CampaignReward",
                            reward_isk,
                            new_bal,
                            format!("Campaign objective {oid} reward"),
                        );
                    }
                }
            });
            if !credited {
                let mut objs = mgr.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
                if let Some(obj) = objs.get_mut(&oid) {
                    if let Some(entry) = obj.contributors.get_mut(&cid) {
                        entry.1 = entry.1.saturating_add(redeemable_units);
                    }
                    obj.reward_pool_isk = obj.reward_pool_isk.saturating_add(payout);
                }
                return Err((STATUS_PRECONDITION_FAILED, "Unable to credit campaign reward".into()));
            }
            mgr.save_to_db();
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.objective.api.RedeemRewardsResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 10. eve_public.campaign.objective.api.GetStatsRequest
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.GetStatsRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let oid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign objective ID".into()))?;
            let objs = mgr.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let obj = objs.get(&oid).ok_or((STATUS_NOT_FOUND, "Campaign objective not found".into()))?;
            let mut out = Vec::new();
            encode_varint_field(1, obj.current_progress as u64, &mut out);
            encode_varint_field(2, obj.committed_participants.len() as u64, &mut out);
            encode_varint_field(3, obj.contributors.len() as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.objective.api.GetStatsResponse"),
                value: out,
            })
        }),
    );

    // 11. eve_public.campaign.objective.api.GetParticipantStatsRequest
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.GetParticipantStatsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let oid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign objective ID".into()))?;
            let objs = mgr.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let obj = objs.get(&oid).ok_or((STATUS_NOT_FOUND, "Campaign objective not found".into()))?;
            let prog = obj.contributors.get(&cid).map(|c| c.0).unwrap_or(0);
            let rank = if prog <= 0 {
                0
            } else {
                1 + obj.contributors.values().filter(|(score, _)| *score > prog).count() as u64
            };
            let mut out = Vec::new();
            encode_varint_field(1, prog as u64, &mut out);
            encode_varint_field(2, rank, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.objective.api.GetParticipantStatsResponse"),
                value: out,
            })
        }),
    );

    // 12. eve_public.campaign.track.api.GetTickingContributionsPredictionRequest
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.track.api.GetTickingContributionsPredictionRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let objs = mgr.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let committed_count: u64 = objs.values().map(|o| o.committed_participants.len() as u64).sum();
            let total_progress: u64 = objs.values().flat_map(|o| o.contributors.values()).map(|(p, _)| (*p).max(0) as u64).sum();
            let mut out = Vec::new();
            if committed_count > 0 || total_progress > 0 {
                encode_varint_field(1, committed_count, &mut out);
                encode_varint_field(2, total_progress, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.track.api.GetTickingContributionsPredictionResponse"),
                value: out,
            })
        }),
    );

    // 13. eve_public.campaign.track.api.RedeemParticipantRewardsRequest
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.track.api.RedeemParticipantRewardsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let oid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign track/objective ID".into()))?;
            let (redeemable_units, payout) = {
                let mut objs = mgr.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
                let obj = objs.get_mut(&oid).ok_or((STATUS_NOT_FOUND, "Campaign track/objective not found".into()))?;
                let pool = obj.reward_pool_isk;
                let unredeemed = obj.contributors.get(&cid).map(|e| e.1).unwrap_or(0);
                let redeemable_units = unredeemed.min(pool / CAMPAIGN_REWARD_ISK_PER_UNIT);
                if unredeemed <= 0 || redeemable_units <= 0 {
                    return Err((STATUS_PRECONDITION_FAILED, "Campaign reward pool is empty or no rewards available".into()));
                }
                let payout = redeemable_units.saturating_mul(CAMPAIGN_REWARD_ISK_PER_UNIT);
                if let Some(entry) = obj.contributors.get_mut(&cid) {
                    entry.1 = entry.1.saturating_sub(redeemable_units);
                }
                obj.reward_pool_isk = obj.reward_pool_isk.saturating_sub(payout);
                (redeemable_units, payout)
            };
            let Some(ref db) = mgr.db else {
                return Err((STATUS_PRECONDITION_FAILED, "Database unavailable for reward payout".into()));
            };
            let reward_isk = eve_db::IskCents::from_isk(payout);
            let mut credited = false;
            db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&(cid as i64)) {
                    if let Some(new_bal) = ch.balance.checked_add(reward_isk) {
                        ch.balance = new_bal;
                        credited = true;
                        snap.append_journal(
                            cid as i64,
                            "CampaignReward",
                            reward_isk,
                            new_bal,
                            format!("Campaign track {oid} participant reward"),
                        );
                    }
                }
            });
            if !credited {
                let mut objs = mgr.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
                if let Some(obj) = objs.get_mut(&oid) {
                    if let Some(entry) = obj.contributors.get_mut(&cid) {
                        entry.1 = entry.1.saturating_add(redeemable_units);
                    }
                    obj.reward_pool_isk = obj.reward_pool_isk.saturating_add(payout);
                }
                return Err((STATUS_PRECONDITION_FAILED, "Unable to credit campaign reward".into()));
            }
            mgr.save_to_db();
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.track.api.RedeemParticipantRewardsResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 14. eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest
    router.register_handler(
        "eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let mut out = Vec::new();
            for (name, params) in AUTHENTIC_TRIGGER_EVENTMATCHES {
                let mut def_bytes = Vec::new();
                encode_trigger_eventmatch_definition(name, params, &mut def_bytes);
                encode_bytes_field(1, &def_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.trigger.eventmatch.definition.api.GetAllResponse"),
                value: out,
            })
        }),
    );
}
