//! `notify_emitters_character_services` — Server-side V24 notification emitters for Character Services
//! (Social, GM/Cluster, Agents, Industry, Seasons/Campaigns, Cosmetics/FastCheckout, Skills, ESI).

#![forbid(unsafe_code)]

use crate::router::notify_registry::{
    OnAchievementsReset, OnAirNpeStateChanged, OnAllowWarUpdated, OnAlphaInjectionAvailable,
    OnCCPNotification, OnCampaignEnrollmentChanged, OnChallengeExpired,
    OnCharacterPermanentlyBanned, OnCharacterTemporarilyBanned, OnCharacterUnbanned,
    OnClusterShutdownCancelled, OnClusterShutdownInitiated, OnCommunityFittingsUpdated,
    OnContextualOfferPurchased, OnCrateLootSpawned, OnCrimewatchEngagementEnded,
    OnDatacoreBought, OnDestinationChange, OnEpicJournalChange, OnFacilitiesUpdated,
    OnFastCheckoutEnabledChanged, OnFleetLootEvent, OnFleetRespawnPointsUpdate,
    OnGlobalConfigUpdated, OnIncomingAgentMessage, OnIncomingTransmission, OnIndustryMaterials,
    OnInvalidateCacheForLocations, OnLocationAttributeChange, OnLoginMilestoneAchieved,
    OnLogonSkillsTrained, OnMilestoneTimerStarted, OnModuleAttributeChange,
};
use crate::router::{NotificationTarget, ServerContext};
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

/// Emits `OnModuleAttributeChange` (8-arg V24 `StateManager.OnModuleAttributeChange`)
/// and, when the item resides inside a ship/container (`loc_id > 0 && loc_id != owner_id`),
/// `OnLocationAttributeChange` (7-arg V24 `StateManager.OnLocationAttributeChange`).
#[allow(clippy::too_many_arguments)]
pub fn emit_setattr_notifications(
    ctx: &ServerContext,
    owner_id: i64,
    loc_id: i64,
    item_id: i64,
    attr_id: u16,
    now_ft: i64,
    val: f64,
    old_val: f64,
) {
    ctx.emit_typed(
        NotificationTarget::Character(owner_id),
        OnModuleAttributeChange {
            owner_id,
            item_id,
            attribute_id: i64::from(attr_id),
            time: now_ft,
            new_value: val,
            old_value: old_val,
            effective_time: now_ft,
            scatter: true,
        },
    );
    if loc_id > 0 && loc_id != owner_id {
        ctx.emit_typed(
            NotificationTarget::Character(owner_id),
            OnLocationAttributeChange {
                owner_id,
                location_id: loc_id,
                item_key: item_id,
                attribute_id: i64::from(attr_id),
                t: now_ft,
                value: val,
                old_value: old_val,
            },
        );
    }
}

/// Broadcasts `OnClusterShutdownCancelled(explanationLabel)` to all online clients.
pub fn emit_cluster_shutdown_cancelled(ctx: &ServerContext, explanation_label: &str) {
    let label = if explanation_label.trim().is_empty() {
        "UI/Shared/ClusterShutdownDelayed"
    } else {
        explanation_label.trim()
    };
    ctx.emit_typed(
        NotificationTarget::All,
        OnClusterShutdownCancelled {
            explanation_label: label.to_string(),
        },
    );
}

/// Broadcasts `OnGlobalConfigUpdated(key, value)` to all online clients.
pub fn emit_global_config_updated(ctx: &ServerContext, key: &str, val: &PyRep) {
    ctx.emit_typed(
        NotificationTarget::All,
        OnGlobalConfigUpdated {
            key: key.to_string(),
            val: val.clone(),
        },
    );
}

/// Emits `OnAirNpeStateChanged(character_id, new_state_id)` (`AirNpeState.SKIPPED = 3`).
pub fn emit_air_npe_skipped(ctx: &ServerContext, char_id: i64) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnAirNpeStateChanged {
            character_id: char_id,
            new_state_id: 3,
        },
    );
}

/// Emits `OnDatacoreBought(characterID, agentID, balance)` upon R&D agent datacore purchase.
pub fn emit_datacore_bought(ctx: &ServerContext, char_id: i64, agent_id: i64, balance_isk: f64) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnDatacoreBought {
            character_id: char_id,
            agent_id,
            balance: balance_isk,
        },
    );
}

/// Emits `OnIncomingAgentMessage`, `OnIncomingTransmission`, and (for Epic Arc agents, `agent_type_id == 10`)
/// `OnEpicJournalChange` upon mission acceptance or completion.
pub fn emit_agent_mission_dialogue(
    ctx: &ServerContext,
    char_id: i64,
    agent_id: i64,
    agent_type_id: u32,
    title: &str,
    body: &str,
    completed: bool,
) {
    let label = if completed {
        "UI/Agents/Dialogue/StandardMissionCompleted"
    } else {
        "UI/Agents/Dialogue/StandardMissionAccepted"
    };
    let clean_header = title.trim_start_matches("[d]").trim();
    let header_str = if clean_header.is_empty() {
        "Agent Transmission"
    } else {
        clean_header
    };
    let text_str = if body.trim().is_empty() {
        header_str
    } else {
        body.trim()
    };
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnIncomingAgentMessage {
            agent_id,
            message: PyRep::Tuple(vec![
                PyRep::String(label.to_string()),
                PyRep::Dict(vec![
                    (
                        PyRep::String("missionName".into()),
                        PyRep::String(header_str.to_string()),
                    ),
                    (PyRep::String("agentID".into()), PyRep::Integer(agent_id)),
                ]),
            ]),
        },
    );
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnIncomingTransmission {
            transmission: PyRep::keyval([
                ("header", PyRep::String(header_str.to_string())),
                ("text", PyRep::String(text_str.to_string())),
                (
                    "icon",
                    PyRep::String("res:/UI/Texture/WindowIcons/agent.png".into()),
                ),
                ("agentID", PyRep::Integer(agent_id)),
            ]),
            is_agent_mission: true,
        },
    );
    if agent_type_id == 10 {
        ctx.emit_typed(NotificationTarget::Character(char_id), OnEpicJournalChange {});
    }
}

/// Emits `OnAllowWarUpdated(corpID, allowWar)` (`id_type = "corpid"`) to each member corporation
/// of `alliance_id` when `allianceRegistry.UpdateAlliance` updates war-eligibility settings.
pub fn emit_allow_war_updated_for_alliance(
    ctx: &ServerContext,
    alliance_id: i64,
    allow_war: bool,
) {
    let member_corps: Vec<i64> = ctx.db.with_snapshot(|snap| {
        let mut corps: Vec<i64> = snap
            .corporations
            .values()
            .filter(|c| c.alliance_id == Some(alliance_id))
            .map(|c| c.corporation_id)
            .collect();
        corps.sort_unstable();
        corps.dedup();
        corps
    });
    ctx.db.with_mut_snapshot(|snap| {
        for &cid in &member_corps {
            snap.fleet_and_social_ops
                .corp_allow_war
                .insert(cid, allow_war);
        }
    });
    for corp_id in member_corps {
        ctx.emit_typed(
            NotificationTarget::Corporation(corp_id),
            OnAllowWarUpdated { corp_id, allow_war },
        );
    }
}

/// Emits `OnInvalidateCacheForLocations(locationIDs)` when `location_ids` is non-empty.
pub fn emit_invalidate_cache_for_locations(
    ctx: &ServerContext,
    target: NotificationTarget,
    location_ids: &[i64],
) {
    if location_ids.is_empty() {
        return;
    }
    ctx.emit_typed(
        target,
        OnInvalidateCacheForLocations {
            location_ids: location_ids.to_vec(),
        },
    );
}

/// Emits `OnCampaignEnrollmentChanged()` when a pilot enrolls in or starts a login campaign.
pub fn emit_campaign_enrollment_changed(ctx: &ServerContext, char_id: i64) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnCampaignEnrollmentChanged {},
    );
}

/// Emits `OnChallengeExpired(challenge_id, new_challenge)` for an expired seasonal challenge.
pub fn emit_challenge_expired(ctx: &ServerContext, char_id: i64, challenge_id: i64, new_kv: PyRep) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnChallengeExpired {
            challenge_id,
            new_challenge: new_kv,
        },
    );
}

/// Emits `OnContextualOfferPurchased(offerID, purchaseInfo)` with real purchase metadata.
pub fn emit_contextual_offer_purchased(ctx: &ServerContext, char_id: i64, offer_id: i64) {
    if offer_id <= 0 {
        return;
    }
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnContextualOfferPurchased {
            offer_id,
            purchase_info: PyRep::Dict(vec![
                (PyRep::String("offerID".into()), PyRep::Integer(offer_id)),
                (PyRep::String("purchasedAt".into()), PyRep::Integer(now_ft)),
                (PyRep::String("confirmed".into()), PyRep::Bool(true)),
            ]),
        },
    );
}

/// Emits `OnCrateLootSpawned()` when opening an inventory crate reveals its loot table.
pub fn emit_crate_loot_spawned(ctx: &ServerContext, char_id: i64) {
    ctx.emit_typed(NotificationTarget::Character(char_id), OnCrateLootSpawned {});
}

/// Emits `OnFastCheckoutEnabledChanged(is_fast_checkout_enabled)` to characters on `user_id`.
pub fn emit_fast_checkout_enabled_changed(ctx: &ServerContext, user_id: u64, enabled: bool) {
    let char_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        let mut ids: Vec<i64> = snap
            .characters
            .values()
            .filter(|c| c.user_id == user_id)
            .map(|c| c.character_id)
            .collect();
        ids.sort_unstable();
        ids
    });
    for cid in char_ids {
        ctx.emit_typed(
            NotificationTarget::Character(cid),
            OnFastCheckoutEnabledChanged {
                is_fast_checkout_enabled: enabled,
            },
        );
    }
}

/// Emits `OnFacilitiesUpdated(facilityIDs)` when facility taxes or settings change.
pub fn emit_facilities_updated(
    ctx: &ServerContext,
    solar_system_id: Option<u32>,
    char_id: i64,
    facility_id: i64,
) {
    if let Some(sys_id) = solar_system_id {
        ctx.emit_typed_with_id_type(
            NotificationTarget::SolarSystem(sys_id),
            "solarsystemid",
            OnFacilitiesUpdated {
                facility_ids: vec![facility_id],
            },
        );
    } else {
        ctx.emit_typed(
            NotificationTarget::Character(char_id),
            OnFacilitiesUpdated {
                facility_ids: vec![facility_id],
            },
        );
    }
}

pub const INDUSTRY_MATERIAL_MONITORS_NOTE_ID: i64 = -8_950_000_000_001_i64;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IndustryMaterialMonitorSub {
    pub char_id: i64,
    pub owner_id: i64,
    pub location_id: i64,
    pub flag_id: i64,
}

pub fn load_industry_material_monitors(
    snap: &eve_db::WorldStateSnapshot,
) -> std::collections::BTreeMap<i64, IndustryMaterialMonitorSub> {
    snap.owner_notes
        .get(&INDUSTRY_MATERIAL_MONITORS_NOTE_ID)
        .and_then(|n| serde_json::from_str(&n.note).ok())
        .unwrap_or_default()
}

pub fn save_industry_material_monitors(
    snap: &mut eve_db::WorldStateSnapshot,
    monitors: &std::collections::BTreeMap<i64, IndustryMaterialMonitorSub>,
) {
    snap.owner_notes.insert(
        INDUSTRY_MATERIAL_MONITORS_NOTE_ID,
        eve_db::OwnerNoteRecord {
            note_id: INDUSTRY_MATERIAL_MONITORS_NOTE_ID,
            character_id: 0,
            item_id: 0,
            label: "industry_material_monitors".into(),
            note: serde_json::to_string(monitors).unwrap_or_default(),
            timestamp_unix: 0,
        },
    );
}

/// Checks whether `sub.char_id` is authorized to query `sub.flag_id` materials in `sub.owner_id`'s hangar.
fn can_query_industry_monitor_div(
    ctx: &ServerContext,
    sub: &IndustryMaterialMonitorSub,
) -> bool {
    if sub.owner_id == sub.char_id {
        return true;
    }
    let div_bits = if (116..=122).contains(&sub.flag_id) {
        let div_idx = (sub.flag_id as u16) - 116;
        (1u64 << (13 + div_idx)) | (1u64 << (20 + div_idx))
    } else {
        0
    };
    let (is_ceo, roles) = crate::services::fleet_social::get_effective_corp_roles(
        ctx,
        sub.owner_id,
        sub.char_id,
    );
    is_ceo
        || roles == u64::MAX
        || (roles
            & (eve_db::corp_roles::ROLE_DIRECTOR
                | eve_db::corp_roles::ROLE_FACTORY_MANAGER
                | div_bits))
            != 0
}

/// Collects total unlocked, non-singleton stack quantities grouped by `type_id`
/// for `(owner_id, location_id, flag_id)`.
pub fn collect_available_monitor_materials(
    snap: &eve_db::WorldStateSnapshot,
    owner_id: i64,
    location_id: i64,
    flag_id: i64,
) -> std::collections::BTreeMap<u32, i64> {
    let mut avail_map = std::collections::BTreeMap::new();
    for it in snap.items.values() {
        if it.owner_id == owner_id
            && it.location_id == location_id
            && (it.flag as i64) == flag_id
            && it.quantity > 0
            && !it.singleton
            && !snap.is_item_or_ancestor_locked_or_impounded(it.item_id)
        {
            *avail_map.entry(it.type_id).or_insert(0) += i64::from(it.quantity);
        }
    }
    avail_map
}

/// Emits `OnIndustryMaterials(monitorID, materials)` (`materials = {typeID: remaining_available_qty}`)
/// per V24 `IndustryService.OnIndustryMaterials` / `Material.update_available` to subscribed monitors
/// when an industry job consumes materials from the monitored hangar/container.
#[allow(clippy::too_many_arguments)]
pub fn emit_industry_materials_consumed(
    ctx: &ServerContext,
    char_id: i64,
    _job_id: i64,
    facility_id: i64,
    docked_id: i64,
    job_owner_id: i64,
    use_corp: bool,
    input_flag: eve_db::InventoryFlag,
    input_container_id: Option<i64>,
    consumed: &[(u32, i64)],
) {
    if consumed.is_empty() {
        return;
    }
    let mut tids: Vec<u32> = consumed
        .iter()
        .filter(|&&(_, qty)| qty > 0)
        .map(|&(tid, _)| tid)
        .collect();
    tids.sort_unstable();
    tids.dedup();
    if tids.is_empty() {
        return;
    }
    let target_loc = input_container_id.unwrap_or(facility_id);
    let target_flag = if use_corp {
        input_flag as i64
    } else {
        eve_db::InventoryFlag::Hangar as i64
    };
    let target_owner = if use_corp { job_owner_id } else { char_id };

    let matching_subs: Vec<(i64, IndustryMaterialMonitorSub)> = ctx.db.with_snapshot(|snap| {
        load_industry_material_monitors(snap)
            .into_iter()
            .filter(|(_, sub)| {
                let loc_match = sub.location_id == target_loc
                    || (input_container_id.is_none() && sub.location_id == docked_id);
                sub.owner_id == target_owner && loc_match && sub.flag_id == target_flag
            })
            .collect()
    });
    if matching_subs.is_empty() {
        return;
    }

    for (monitor_id, sub) in matching_subs {
        if !can_query_industry_monitor_div(ctx, &sub) {
            continue;
        }
        let entries: Vec<(PyRep, PyRep)> = ctx.db.with_snapshot(|snap| {
            let avail_map = collect_available_monitor_materials(
                snap,
                sub.owner_id,
                sub.location_id,
                sub.flag_id,
            );
            tids.iter()
                .map(|&tid| {
                    let avail = avail_map.get(&tid).copied().unwrap_or(0);
                    (PyRep::Integer(i64::from(tid)), PyRep::Integer(avail))
                })
                .collect()
        });
        ctx.emit_typed(
            NotificationTarget::Character(sub.char_id),
            OnIndustryMaterials {
                job_id: monitor_id,
                materials: PyRep::Dict(entries),
            },
        );
    }
}

/// Emits `OnIndustryMaterials(monitorID, materials)` to active `industryMonitor` subscribers
/// whose `(owner_id, location_id, flag_id)` matches any affected item scope `(owner_id, location_id, flag_id, type_id)`.
///
/// Because V24 `IndustryService.OnIndustryMaterials` assigns `job.available = materials` (replacing
/// the full dict rather than merging keys), the emitted dict includes all currently available
/// non-singleton unlocked items in the monitored scope plus `0` for affected `type_id`s with none left.
pub fn emit_industry_materials_for_scopes(
    ctx: &ServerContext,
    scopes: &[(i64, i64, i64, u32)],
) {
    if scopes.is_empty() {
        return;
    }
    let matching_subs: Vec<(i64, IndustryMaterialMonitorSub)> = ctx.db.with_snapshot(|snap| {
        let monitors = load_industry_material_monitors(snap);
        if monitors.is_empty() {
            return Vec::new();
        }
        monitors
            .into_iter()
            .filter(|(_, sub)| {
                scopes.iter().any(|&(owner_id, loc_id, flag_id, _)| {
                    sub.owner_id == owner_id
                        && sub.location_id == loc_id
                        && sub.flag_id == flag_id
                })
            })
            .collect()
    });
    if matching_subs.is_empty() {
        return;
    }

    for (monitor_id, sub) in matching_subs {
        if !can_query_industry_monitor_div(ctx, &sub) {
            continue;
        }
        let entries: Vec<(PyRep, PyRep)> = ctx.db.with_snapshot(|snap| {
            let mut avail_map = collect_available_monitor_materials(
                snap,
                sub.owner_id,
                sub.location_id,
                sub.flag_id,
            );
            for &(owner_id, loc_id, flag_id, tid) in scopes {
                if owner_id == sub.owner_id
                    && loc_id == sub.location_id
                    && flag_id == sub.flag_id
                {
                    avail_map.entry(tid).or_insert(0);
                }
            }
            avail_map
                .into_iter()
                .map(|(tid, qty)| (PyRep::Integer(i64::from(tid)), PyRep::Integer(qty)))
                .collect()
        });
        ctx.emit_typed(
            NotificationTarget::Character(sub.char_id),
            OnIndustryMaterials {
                job_id: monitor_id,
                materials: PyRep::Dict(entries),
            },
        );
    }
}

/// Emits `OnLogonSkillsTrained(skillInfos, canTrain)` when offline skill queue catch-up finishes >= 1 skill.
pub fn emit_logon_skills_trained(
    ctx: &ServerContext,
    char_id: i64,
    trained: &[(u32, u8, u8, f64)],
    can_train: bool,
) {
    if trained.is_empty() {
        return;
    }
    let mut skill_map: HashMap<u32, (u8, u8, f64)> = HashMap::new();
    for &(tid, prev_lvl, new_lvl, sp) in trained {
        skill_map
            .entry(tid)
            .and_modify(|e| {
                e.1 = e.1.max(new_lvl);
                e.2 = e.2.max(sp);
            })
            .or_insert((prev_lvl, new_lvl, sp));
    }
    let mut keys: Vec<u32> = skill_map.keys().copied().collect();
    keys.sort_unstable();
    let entries: Vec<(PyRep, PyRep)> = keys
        .into_iter()
        .map(|tid| {
            let (prev_lvl, new_lvl, sp) = skill_map[&tid];
            (
                PyRep::Integer(i64::from(tid)),
                PyRep::keyval([
                    ("currentLevel", PyRep::Integer(i64::from(prev_lvl))),
                    ("skillLevel", PyRep::Integer(i64::from(new_lvl))),
                    ("skillPoints", PyRep::Real(sp)),
                ]),
            )
        })
        .collect();
    if entries.is_empty() {
        return;
    }
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnLogonSkillsTrained {
            skill_infos: PyRep::Dict(entries),
            can_train,
        },
    );
}

/// Emits `OnDestinationChange(solarSystemID, clearOtherWaypoints, first)` on ESI waypoint update.
pub fn emit_destination_change(
    ctx: &ServerContext,
    char_id: i64,
    destination_id: i64,
    clear_other_waypoints: bool,
    add_to_beginning: bool,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnDestinationChange {
            solar_system_id: destination_id,
            clear_other_waypoints,
            first: add_to_beginning,
        },
    );
}

/// Emits `OnFleetRespawnPointsUpdate(respawnPoints)` to `fleet_id` when accessible clone bays/vats exist.
pub fn emit_fleet_respawn_points_if_any(ctx: &ServerContext, fleet_id: i64) {
    if fleet_id <= 0 {
        return;
    }
    let points: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
        let Some(fleet) = snap.fleets.get(&fleet_id) else {
            return Vec::new();
        };
        let boss_corp = snap
            .characters
            .get(&fleet.boss_id)
            .map(|c| c.corporation_id)
            .unwrap_or(0);
        let mut structure_ids: Vec<i64> = snap
            .structures
            .values()
            .filter(|st| {
                st.owner_corp_id == boss_corp
                    && st.state != "destroyed"
                    && st.state != "unanchored"
                    && snap.items.values().any(|i| {
                        i.location_id == st.structure_id
                            && (i.type_id == 35_894
                                || ctx
                                    .sde
                                    .types
                                    .get(&i.type_id)
                                    .is_some_and(|t| t.group_id == 1415 || t.name.contains("Cloning Center")))
                            && i.flag.is_service_slot()
                            && snap.module_online_states.get(&i.item_id).copied().unwrap_or(false)
                    })
            })
            .map(|st| st.structure_id)
            .collect();
        structure_ids.sort_unstable();
        structure_ids
            .into_iter()
            .filter_map(|sid| {
                let st = snap.structures.get(&sid)?;
                Some(PyRep::keyval([
                    ("solarsystemID", PyRep::Integer(i64::from(st.solar_system_id))),
                    ("extraInfo", PyRep::Integer(st.structure_id)),
                    ("locationID", PyRep::Integer(st.structure_id)),
                    ("typeID", PyRep::Integer(i64::from(st.type_id))),
                ]))
            })
            .collect()
    });
    if points.is_empty() {
        return;
    }
    ctx.emit_typed(
        NotificationTarget::Fleet(fleet_id),
        OnFleetRespawnPointsUpdate {
            respawn_points: PyRep::List(points),
        },
    );
}

/// Emits `OnFleetLootEvent(lootEvents)` (`{(charID, typeID): qty}`) when a fleet member loots from space.
pub fn emit_fleet_loot_event_if_in_fleet(
    ctx: &ServerContext,
    char_id: i64,
    fleet_id_opt: Option<i64>,
    looted_items: &[(u32, i32)],
) {
    let Some(fleet_id) = fleet_id_opt.filter(|&f| f > 0) else {
        return;
    };
    let mut counts: HashMap<u32, i64> = HashMap::new();
    for &(tid, qty) in looted_items {
        if qty > 0 {
            *counts.entry(tid).or_insert(0) += i64::from(qty);
        }
    }
    if counts.is_empty() {
        return;
    }
    let mut tids: Vec<u32> = counts.keys().copied().collect();
    tids.sort_unstable();
    let entries: Vec<(PyRep, PyRep)> = tids
        .into_iter()
        .map(|tid| {
            (
                PyRep::Tuple(vec![
                    PyRep::Integer(char_id),
                    PyRep::Integer(i64::from(tid)),
                ]),
                PyRep::Integer(counts[&tid]),
            )
        })
        .collect();
    ctx.emit_typed(
        NotificationTarget::Fleet(fleet_id),
        OnFleetLootEvent {
            loot_events: PyRep::Dict(entries),
        },
    );
}

/// Emits `OnAlphaInjectionAvailable()` when daily alpha injection becomes available for `char_id`.
pub fn emit_alpha_injection_available(ctx: &ServerContext, char_id: i64) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnAlphaInjectionAvailable {},
    );
}

/// Emits `OnCCPNotification(title, subtitle, text, notificationTypeID, language, reference_id)`.
pub fn emit_ccp_notification(
    ctx: &ServerContext,
    target: NotificationTarget,
    title: &str,
    subtitle: &str,
    text: &str,
    notification_type_id: i64,
    reference_id: i64,
) {
    ctx.emit_typed(
        target,
        OnCCPNotification {
            title: title.to_string(),
            subtitle: subtitle.to_string(),
            text: text.to_string(),
            notification_type_id,
            language: None,
            reference_id,
        },
    );
}

/// Broadcasts `OnCommunityFittingsUpdated()` when community fittings are modified.
pub fn emit_community_fittings_updated(ctx: &ServerContext) {
    ctx.emit_typed(NotificationTarget::All, OnCommunityFittingsUpdated {});
}

/// Emits `OnLoginMilestoneAchieved(milestoneID, iskAmount)`.
pub fn emit_login_milestone_achieved(
    ctx: &ServerContext,
    char_id: i64,
    milestone_id: i64,
    isk_amount: f64,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnLoginMilestoneAchieved {
            milestone_id,
            isk_amount,
        },
    );
}

/// Emits `OnMilestoneTimerStarted(timeLeft)`.
pub fn emit_milestone_timer_started(ctx: &ServerContext, char_id: i64, time_left: i64) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnMilestoneTimerStarted { time_left },
    );
}

const CHAR_ID_PREFIX_MIN: i64 = 3_000_000;
const UPWELL_STRUCTURE_ID_MIN: i64 = 1_000_000_000_000;

fn resolve_optional_target_char<'a, 'b>(
    ctx: &ServerContext,
    caller_cid: i64,
    args: &'a [&'b str],
    cmd_head: &str,
) -> Result<(i64, &'a [&'b str]), BridgeRpcError> {
    let Some(first) = args.first().copied() else {
        return Ok((caller_cid, args));
    };
    if first.eq_ignore_ascii_case("me") || first.eq_ignore_ascii_case("self") {
        return Ok((caller_cid, &args[1..]));
    }
    if let Ok(parsed) = first.parse::<i64>() {
        if parsed > 0 {
            let (is_char, is_known_domain_id) = ctx.db.with_snapshot(|s| {
                let is_char = parsed == caller_cid || s.characters.contains_key(&parsed);
                let is_known_domain_id = match cmd_head {
                    "/upwelltimer" | "/structuretimer" | "/finishstructure" => {
                        parsed >= UPWELL_STRUCTURE_ID_MIN || s.structures.contains_key(&parsed)
                    }
                    "/completeindustry" | "/finishjobs" | "/industrytimer" => {
                        s.industry_jobs.contains_key(&parsed)
                    }
                    _ => false,
                };
                (is_char, is_known_domain_id)
            });
            if is_char {
                return Ok((parsed, &args[1..]));
            }
            if parsed >= CHAR_ID_PREFIX_MIN && !is_known_domain_id {
                return Err(BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: format!("Character {parsed} not found"),
                    client_error: None,
                });
            }
        }
    }
    Ok((caller_cid, args))
}

fn resolve_target_solar_system(
    ctx: &ServerContext,
    caller_cid: i64,
    caller_sys: Option<u32>,
    target_cid: i64,
) -> u32 {
    if target_cid != caller_cid {
        ctx.db
            .with_snapshot(|s| s.characters.get(&target_cid).map(|c| c.solar_system_id))
            .filter(|&s| s > 0)
            .or(caller_sys)
            .unwrap_or(0)
    } else {
        caller_sys
            .or_else(|| {
                ctx.db
                    .with_snapshot(|s| s.characters.get(&caller_cid).map(|c| c.solar_system_id))
            })
            .unwrap_or(0)
    }
}

/// Handles Batch 3 GM/QA slash commands (`/achievements reset`, `/chatban`, `/chatunban`,
/// `/campaign enroll`, `/shutdown cancel`, `/fastcheckout`, `/fwcapture`, `/spawnanomaly`,
/// and timer acceleration commands `/advancetime`, `/completeskills`, `/upwelltimer`,
/// `/completeindustry`, `/cleartimers` plus their aliases).
pub fn handle_batch3_gm_slash_cmd(
    ctx: &ServerContext,
    caller_cid: i64,
    caller_sys: Option<u32>,
    cmd: &str,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    let head = parts.first().copied()?.to_ascii_lowercase();
    match head.as_str() {
        "/achievements" if parts.get(1).is_some_and(|s| s.eq_ignore_ascii_case("reset")) => {
            let target_cid = parts
                .get(2)
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(caller_cid);
            ctx.quasar_gateway.progression.achievements.clear_character(target_cid as u32);
            ctx.db.with_mut_snapshot(|snap| {
                let mut ext = crate::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap);
                ext.achievements_by_char.remove(&target_cid);
                ext.achievement_counters_by_char.remove(&target_cid);
                crate::services::cosmetics_ui_ops::save_cosmetics_ui_state(snap, &ext);
            });
            ctx.emit_typed(
                NotificationTarget::Character(target_cid),
                OnAchievementsReset {},
            );
            Some(Ok(PyRep::keyval([
                ("command", PyRep::String(cmd.to_string())),
                ("characterID", PyRep::Integer(target_cid)),
                ("reset", PyRep::Bool(true)),
            ])))
        }
        "/chatban" => {
            let Some(target_cid) = parts.get(1).and_then(|s| s.parse::<i64>().ok()).filter(|&id| id > 0) else {
                return Some(Err(BridgeRpcError::user_error("InvalidSlashArguments")));
            };
            let Ok(cid_u32) = u32::try_from(target_cid) else {
                return Some(Err(BridgeRpcError::user_error("InvalidSlashArguments")));
            };
            let mode = parts.get(2).copied().unwrap_or("3600");
            let is_perm = mode.eq_ignore_ascii_case("perm") || mode.eq_ignore_ascii_case("permanent");
            let now_unix = ctx.now_unix();
            let now_ft = ctx.now_blueos_filetime_ticks();
            let (expiry_unix, expiry_ft, reason) = if is_perm {
                (u64::MAX, i64::MAX, "Permanent GM chat ban".to_string())
            } else {
                let secs = mode.parse::<i64>().unwrap_or(3600).max(1);
                (
                    now_unix.saturating_add(secs as u64),
                    now_ft.saturating_add(secs.saturating_mul(10_000_000)),
                    "Temporary GM chat ban".to_string(),
                )
            };
            let mutes_snapshot = {
                let mut mutes = ctx
                    .quasar_gateway
                    .chat_moderation
                    .mutes
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                mutes.retain(|_, (exp, _)| *exp > now_unix);
                mutes.insert(cid_u32, (expiry_unix, reason));
                mutes.clone()
            };
            crate::services::quasar_social_chat::save_mutes_to_db(&ctx.db, &mutes_snapshot);
            if is_perm {
                ctx.emit_typed(
                    NotificationTarget::Character(target_cid),
                    OnCharacterPermanentlyBanned {},
                );
            } else {
                ctx.emit_typed(
                    NotificationTarget::Character(target_cid),
                    OnCharacterTemporarilyBanned {
                        ban_expiry: expiry_ft,
                    },
                );
            }
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(target_cid)),
                ("permanent", PyRep::Bool(is_perm)),
                ("banExpiry", PyRep::Integer(expiry_ft)),
            ])))
        }
        "/chatunban" => {
            let Some(target_cid) = parts.get(1).and_then(|s| s.parse::<i64>().ok()).filter(|&id| id > 0) else {
                return Some(Err(BridgeRpcError::user_error("InvalidSlashArguments")));
            };
            if let Ok(cid_u32) = u32::try_from(target_cid) {
                let mutes_snapshot = {
                    let mut mutes = ctx
                        .quasar_gateway
                        .chat_moderation
                        .mutes
                        .write()
                        .unwrap_or_else(|e| e.into_inner());
                    mutes.remove(&cid_u32);
                    mutes.clone()
                };
                crate::services::quasar_social_chat::save_mutes_to_db(&ctx.db, &mutes_snapshot);
            }
            ctx.emit_typed(
                NotificationTarget::Character(target_cid),
                OnCharacterUnbanned {},
            );
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(target_cid)),
                ("unbanned", PyRep::Bool(true)),
            ])))
        }
        "/campaign" if parts.get(1).is_some_and(|s| s.eq_ignore_ascii_case("enroll")) => {
            let Some(campaign_id) = parts
                .get(2)
                .and_then(|s| s.parse::<i64>().ok())
                .filter(|&id| id > 0)
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSlashArguments".into(),
                    message: "Usage: /campaign enroll <campaign_id> [character_id]".into(),
                    client_error: None,
                }));
            };
            let target_cid = match parts.get(3) {
                Some(raw) => match raw.parse::<i64>().ok().filter(|&id| id > 0) {
                    Some(id) => id,
                    None => {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidSlashArguments".into(),
                            message: "character_id must be a positive integer".into(),
                            client_error: None,
                        }))
                    }
                },
                None if caller_cid > 0 => caller_cid,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArguments".into(),
                        message: "character_id is required when no character is selected".into(),
                        client_error: None,
                    }))
                }
            };
            let sde_campaigns = crate::services::campaigns_and_milestones::get_sde_login_campaigns(&ctx.sde);
            if !sde_campaigns.is_empty() && !sde_campaigns.contains_key(&campaign_id) {
                return Some(Err(BridgeRpcError {
                    code: "CampaignNotFound".into(),
                    message: format!("Login campaign {campaign_id} not found in SDE seasonalcampaignsloginrewards"),
                    client_error: None,
                }));
            }
            ctx.db.with_mut_snapshot(|snap| {
                snap.seasons_and_campaigns
                    .login_campaign_enrollments
                    .insert(target_cid, campaign_id);
            });
            emit_campaign_enrollment_changed(ctx, target_cid);
            Some(Ok(PyRep::keyval([
                ("campaignID", PyRep::Integer(campaign_id)),
                ("characterID", PyRep::Integer(target_cid)),
                ("enrolled", PyRep::Bool(true)),
            ])))
        }
        "/campaign" if parts.get(1).is_some_and(|s| s.eq_ignore_ascii_case("seasonal")) => {
            use crate::services::campaigns_and_milestones::{
                format_turnover_hms, get_sde_login_campaigns, parse_turnover_hms,
                DEFAULT_SEASONAL_TURNOVER_SECS,
            };
            use crate::services::cosmetics_ui_ops::{
                load_cosmetics_ui_state, save_cosmetics_ui_state, SeasonalCampaignSchedule,
            };
            if parts.get(2).is_some_and(|s| s.eq_ignore_ascii_case("off") || s.eq_ignore_ascii_case("stop")) {
                ctx.db.with_mut_snapshot(|snap| {
                    let mut ext = load_cosmetics_ui_state(snap);
                    ext.active_seasonal_schedule = None;
                    ext.active_seasonal_campaign_id = None;
                    save_cosmetics_ui_state(snap, &ext);
                });
                emit_campaign_enrollment_changed(ctx, caller_cid);
                return Some(Ok(PyRep::keyval([("active", PyRep::Bool(false))])));
            }
            let Some(campaign_id) = parts.get(2).and_then(|s| s.parse::<i64>().ok()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSlashArguments".into(),
                    message: "Usage: /campaign seasonal <campaign_id|off> [duration_days] [HH:MM:SS]".into(),
                    client_error: None,
                }));
            };
            if !get_sde_login_campaigns(&ctx.sde).contains_key(&campaign_id) {
                return Some(Err(BridgeRpcError {
                    code: "CampaignNotFound".into(),
                    message: format!("Seasonal campaign {campaign_id} not found in SDE seasonalcampaignsloginrewards"),
                    client_error: None,
                }));
            }
            let duration_days = match parts.get(3) {
                Some(raw) => match raw.parse::<u64>().ok().filter(|d| (1..=365).contains(d)) {
                    Some(d) => d,
                    None => {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidSlashArguments".into(),
                            message: "duration_days must be in 1..=365".into(),
                            client_error: None,
                        }))
                    }
                },
                None => 30,
            };
            let turnover_secs = match parts.get(4) {
                Some(hms) => match parse_turnover_hms(hms) {
                    Some(secs) => secs,
                    None => {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidSlashArguments".into(),
                            message: "turnover time must match HH:MM:SS".into(),
                            client_error: None,
                        }))
                    }
                },
                None => DEFAULT_SEASONAL_TURNOVER_SECS,
            };
            let now = ctx.now_unix();
            let end_unix = now.saturating_add(duration_days * 86_400);
            ctx.db.with_mut_snapshot(|snap| {
                let mut ext = load_cosmetics_ui_state(snap);
                ext.active_seasonal_campaign_id = Some(campaign_id);
                ext.active_seasonal_schedule = Some(SeasonalCampaignSchedule {
                    campaign_id,
                    start_unix: now,
                    end_unix,
                    turnover_secs,
                });
                save_cosmetics_ui_state(snap, &ext);
            });
            emit_campaign_enrollment_changed(ctx, caller_cid);
            Some(Ok(PyRep::keyval([
                ("campaignID", PyRep::Integer(campaign_id)),
                ("startUnix", PyRep::Integer(now as i64)),
                ("endUnix", PyRep::Integer(end_unix as i64)),
                ("turnoverTime", PyRep::String(format_turnover_hms(turnover_secs))),
            ])))
        }
        "/shutdown" if parts.get(1).is_some_and(|s| s.eq_ignore_ascii_case("cancel")) => {
            let label = parts
                .get(2)
                .copied()
                .unwrap_or("UI/Shared/ClusterShutdownDelayed");
            {
                let mut st = ctx.cluster_state.write().unwrap_or_else(|e| e.into_inner());
                st.shutdown_in_progress = false;
            }
            emit_cluster_shutdown_cancelled(ctx, label);
            Some(Ok(PyRep::keyval([
                ("cancelled", PyRep::Bool(true)),
                ("explanationLabel", PyRep::String(label.to_string())),
            ])))
        }
        "/shutdown" if parts.get(1).is_some_and(|s| s.parse::<u64>().is_ok()) => {
            let secs: u64 = parts[1].parse::<u64>().unwrap_or(60).max(60).min(86400 * 30);
            let reason = parts.get(2).copied().unwrap_or("ServerRestart");
            let now_ft = ctx.now_blueos_filetime_ticks();
            let target_ft = now_ft.saturating_add((secs as i64).saturating_mul(10_000_000));
            {
                let mut st = ctx.cluster_state.write().unwrap_or_else(|e| e.into_inner());
                st.shutdown_in_progress = true;
            }
            let explanation = if reason.contains("Delay") {
                "UI/Shared/ClusterShutdownDelayed"
            } else {
                "UI/Shared/ShutdownNotificationTitle"
            };
            ctx.emit_typed(
                NotificationTarget::All,
                OnClusterShutdownInitiated {
                    explanation_label: explanation.into(),
                    when: target_ft,
                    duration: secs as i64,
                },
            );
            Some(Ok(PyRep::keyval([
                ("shutdownInitiated", PyRep::Bool(true)),
                ("targetFt", PyRep::Integer(target_ft)),
            ])))
        }
        "/fastcheckout" => {
            let enabled = parts
                .get(1)
                .map(|s| *s == "1" || s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("on"))
                .unwrap_or(true);
            let uid = ctx
                .db
                .with_snapshot(|s| s.characters.get(&caller_cid).map(|c| c.user_id))
                .unwrap_or(0);
            if uid > 0 {
                let prev = crate::services::fast_checkout_and_activity::get_user_fast_checkout_state(ctx, uid).enabled;
                if prev != enabled {
                    crate::services::fast_checkout_and_activity::set_user_fast_checkout_enabled(ctx, uid, enabled);
                    emit_fast_checkout_enabled_changed(ctx, uid, enabled);
                }
            }
            Some(Ok(PyRep::Bool(enabled)))
        }
        "/spawnanomaly" => {
            let (target_cid, rem) =
                match resolve_optional_target_char(ctx, caller_cid, &parts[1..], head.as_str()) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
            let sys_id = resolve_target_solar_system(ctx, caller_cid, caller_sys, target_cid);
            let dungeon_id = rem
                .first()
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(10_001);
            let (anom_id, inst_id) =
                super::notify_emitters_space_dungeons::spawn_and_emit_cosmic_anomaly(ctx, sys_id, dungeon_id);
            Some(Ok(PyRep::keyval([
                ("targetID", PyRep::String(anom_id)),
                ("instanceID", PyRep::Integer(inst_id)),
                ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
            ])))
        }
        "/fwcapture" => {
            let sys_id = parts
                .get(1)
                .and_then(|s| s.parse::<u32>().ok())
                .or(caller_sys)
                .unwrap_or(0);
            let vp = parts
                .get(2)
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(75_000);
            let (new_vp, contested) = crate::services::fw_services::add_victory_points(ctx, sys_id, vp);
            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                ("victoryPoints", PyRep::Integer(new_vp)),
                ("contestedPercentage", PyRep::Real(contested)),
            ])))
        }
        "/advancetime" | "/fastforward" | "/ff" => {
            let (target_cid, secs) = {
                let (mut cid_res, rem) =
                    match resolve_optional_target_char(ctx, caller_cid, &parts[1..], head.as_str()) {
                        Ok(v) => v,
                        Err(e) => return Some(Err(e)),
                    };
                let mut parsed_secs: Option<u64> = None;
                for tok in rem {
                    if tok.eq_ignore_ascii_case("me") || tok.eq_ignore_ascii_case("self") {
                        continue;
                    }
                    if cid_res == caller_cid {
                        if let Ok(parsed) = tok.parse::<i64>() {
                            if parsed > 0
                                && (parsed == caller_cid
                                    || ctx.db.with_snapshot(|s| s.characters.contains_key(&parsed)))
                            {
                                cid_res = parsed;
                                continue;
                            }
                            if parsed >= CHAR_ID_PREFIX_MIN {
                                return Some(Err(BridgeRpcError {
                                    code: "CharacterNotFound".into(),
                                    message: format!("Character {parsed} not found"),
                                    client_error: None,
                                }));
                            }
                        }
                    }
                    if parsed_secs.is_none() {
                        if let Ok(s) = tok.parse::<u64>() {
                            parsed_secs = Some(s);
                        }
                    }
                }
                (cid_res, parsed_secs.unwrap_or(3600))
            };
            // #1005 / #1015: Advance server timers without emitting `DoSimClockRebase` so live
            // client `Ballpark.AdjustTimes` and `blue.os` clocks are not pushed into the future
            // (`router/dispatch.rs` `timeControl.AdvanceClock` parity). Callers in async slash
            // paths (`character_gm_tools::handle_gm_slash_ext`) also invoke
            // `fast_forward_and_sync_space_clock` so live `SystemActor` capacitor/shield and HUD sync.
            ctx.advance_secs_without_sim_clock_rebase(secs);
            crate::services::space_destiny::clear_session_change_timer(ctx, target_cid);
            Some(Ok(PyRep::keyval([
                ("command", PyRep::String(cmd.to_string())),
                ("characterID", PyRep::Integer(target_cid)),
                ("advancedSeconds", PyRep::Integer(secs as i64)),
                ("nowUnix", PyRep::Integer(ctx.now_unix() as i64)),
            ])))
        }
        "/completeskills" | "/trainskills" | "/finishskills" => {
            let (target_cid, rem) =
                match resolve_optional_target_char(ctx, caller_cid, &parts[1..], head.as_str()) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
            let arg = rem.first().copied().unwrap_or("all");
            let advance_s: f64 = if arg.eq_ignore_ascii_case("all") {
                31_536_000.0 * 5.0
            } else if let Ok(secs) = arg.parse::<u64>() {
                secs as f64
            } else {
                31_536_000.0 * 5.0
            };
            let (pre_queue_len, pre_skill_state): (usize, Vec<(u32, u8, f64)>) =
                ctx.db.with_mut_snapshot(|snap| {
                    snap.paused_skill_queues.remove(&target_cid);
                    let mut tids = Vec::new();
                    let qlen = snap
                        .skill_queues
                        .get(&target_cid)
                        .map(|q| {
                            for e in q {
                                if !tids.contains(&e.type_id) {
                                    tids.push(e.type_id);
                                }
                            }
                            q.len()
                        })
                        .unwrap_or(0);
                    let skills = snap.skills.get(&target_cid);
                    let state = tids
                        .into_iter()
                        .map(|tid| {
                            let (lvl, sp) = skills
                                .and_then(|m| m.get(&tid))
                                .map(|r| (r.level, r.skill_points))
                                .unwrap_or((0, 0.0));
                            (tid, lvl, sp)
                        })
                        .collect();
                    (qlen, state)
                });
            ctx.advance_skill_queues(advance_s);
            let (rem_q, total_sp, completed_any, partial_tids) = ctx.db.with_snapshot(|snap| {
                let qlen = snap
                    .skill_queues
                    .get(&target_cid)
                    .map(|q| q.len())
                    .unwrap_or(0);
                let sp = snap
                    .characters
                    .get(&target_cid)
                    .map(|c| c.skill_points)
                    .unwrap_or(0.0);
                let skills = snap.skills.get(&target_cid);
                let mut completed_any = qlen < pre_queue_len;
                let mut partial_tids = Vec::new();
                for (tid, prev_lvl, prev_sp) in &pre_skill_state {
                    let (cur_lvl, cur_sp) = skills
                        .and_then(|m| m.get(tid))
                        .map(|r| (r.level, r.skill_points))
                        .unwrap_or((0, 0.0));
                    if cur_lvl > *prev_lvl {
                        completed_any = true;
                    } else if cur_sp > *prev_sp {
                        partial_tids.push(*tid);
                    }
                }
                (qlen, sp, completed_any, partial_tids)
            });
            if !completed_any {
                if !partial_tids.is_empty() {
                    crate::services::jump_clone_names::emit_server_skills_changed(
                        ctx,
                        target_cid,
                        &partial_tids,
                        Some("SkillTrained"),
                    );
                }
                let _ = ctx.notify_new_skill_queue_saved(
                    target_cid,
                    crate::services::jump_clone_names::skill_queue_rowset(ctx, target_cid),
                );
            }
            Some(Ok(PyRep::keyval([
                ("command", PyRep::String(cmd.to_string())),
                ("characterID", PyRep::Integer(target_cid)),
                ("remainingQueueLen", PyRep::Integer(rem_q as i64)),
                ("totalSkillPoints", PyRep::Real(total_sp)),
            ])))
        }
        "/upwelltimer" | "/structuretimer" | "/finishstructure" => {
            let (target_cid, rem) =
                match resolve_optional_target_char(ctx, caller_cid, &parts[1..], head.as_str()) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
            let target_sid_opt = rem.first().and_then(|s| s.parse::<i64>().ok());
            let now = ctx.now_unix();
            let sys_id = resolve_target_solar_system(ctx, caller_cid, caller_sys, target_cid);
            let matched_ids: Vec<i64> = ctx.db.with_mut_snapshot(|snap| {
                let mut sids: Vec<i64> = snap
                    .structures
                    .values()
                    .filter(|st| match target_sid_opt {
                        Some(sid) => st.structure_id == sid,
                        None => st.solar_system_id == sys_id,
                    })
                    .map(|st| st.structure_id)
                    .collect();
                sids.sort_unstable();
                let mut ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
                for &sid in &sids {
                    if let Some(st) = snap.structures.get_mut(&sid) {
                        if st.reinforced_until_unix > now {
                            st.reinforced_until_unix = now;
                        }
                        if let Some(sd) = st.self_destruct_at_unix {
                            if sd > now {
                                st.self_destruct_at_unix = Some(now);
                            }
                        }
                    }
                    if let Some(u) = ext.unanchoring_until_unix.get_mut(&sid) {
                        if *u > now {
                            *u = now;
                        }
                    }
                    if let Some(life) = ext.lifecycle.get_mut(&sid) {
                        if life.repair_until_unix.is_some_and(|t| t > now) {
                            life.repair_until_unix = Some(now);
                        }
                    }
                }
                crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
                sids
            });
            let _ = crate::services::station_structure::structure_lifecycle::sync_all_structures(ctx);
            let transitions: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
                matched_ids
                    .iter()
                    .map(|&sid| {
                        let st_state = snap
                            .structures
                            .get(&sid)
                            .map(|s| s.state.clone())
                            .unwrap_or_else(|| "PackagedOrRemoved".into());
                        PyRep::keyval([
                            ("structureID", PyRep::Integer(sid)),
                            ("newState", PyRep::String(st_state)),
                        ])
                    })
                    .collect()
            });
            Some(Ok(PyRep::keyval([
                ("command", PyRep::String(cmd.to_string())),
                ("updatedCount", PyRep::Integer(transitions.len() as i64)),
                ("structures", PyRep::List(transitions)),
            ])))
        }
        "/structure" => {
            if parts.len() != 4 || !parts.get(1).is_some_and(|s| s.eq_ignore_ascii_case("state"))
            {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSlashArgument".into(),
                    message: "Usage: /structure state <structure_id> <state_id>".into(),
                    client_error: None,
                }));
            }
            let (Some(structure_id), Some(state_id)) = (
                parts.get(2).and_then(|s| s.parse::<i64>().ok()),
                parts.get(3).and_then(|s| s.parse::<i64>().ok()),
            ) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSlashArgument".into(),
                    message: "Usage: /structure state <structure_id> <state_id>".into(),
                    client_error: None,
                }));
            };
            match crate::services::station_structure::structure_lifecycle::gm_set_structure_state(
                ctx,
                structure_id,
                state_id,
            ) {
                Ok((old_state, new_state)) => Some(Ok(PyRep::keyval([
                    ("command", PyRep::String(cmd.to_string())),
                    ("structureID", PyRep::Integer(structure_id)),
                    ("stateID", PyRep::Integer(state_id)),
                    ("oldState", PyRep::String(old_state)),
                    ("newState", PyRep::String(new_state)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }
        "/completeindustry" | "/finishjobs" | "/industrytimer" => {
            let (target_cid, rem) =
                match resolve_optional_target_char(ctx, caller_cid, &parts[1..], head.as_str()) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
            let target_job = rem.first().and_then(|s| s.parse::<i64>().ok());
            let now = ctx.now_unix();
            let corp_id = ctx
                .db
                .with_snapshot(|s| s.characters.get(&target_cid).map(|c| c.corporation_id))
                .unwrap_or(0);
            let completed_jobs: Vec<(i64, i64, i64, i64, u32)> = ctx.db.with_mut_snapshot(|snap| {
                let mut matched = Vec::new();
                for job in snap.industry_jobs.values_mut() {
                    if !matches!(job.status.as_str(), "Active" | "Ready") {
                        continue;
                    }
                    let matches = match target_job {
                        Some(jid) => job.job_id == jid,
                        None => job.installer_id == target_cid || job.installer_id == corp_id,
                    };
                    if matches {
                        job.status = "Ready".into();
                        job.start_unix = now.saturating_sub(job.duration_seconds.max(1));
                        matched.push((job.job_id, job.installer_id, job.blueprint_item_id, job.runs));
                    }
                }
                matched.sort_unstable_by_key(|(jid, _, _, _)| *jid);
                matched
                    .into_iter()
                    .map(|(jid, installer_id, bp_iid, runs)| {
                        let owner_id = crate::services::industry_pi::load_job_cost_meta(snap, jid)
                            .map(|m| m.owner_id)
                            .filter(|&id| id > 0)
                            .unwrap_or(installer_id);
                        (jid, installer_id, owner_id, bp_iid, runs)
                    })
                    .collect()
            });
            for (jid, installer_id, owner_id, bp_iid, runs) in &completed_jobs {
                crate::services::industry_pi::emit_industry_job(
                    ctx,
                    *installer_id,
                    *owner_id,
                    *jid,
                    *bp_iid,
                    3,
                    *runs as i64,
                );
                if target_cid != *installer_id {
                    crate::services::industry_pi::emit_industry_job(
                        ctx,
                        target_cid,
                        *owner_id,
                        *jid,
                        *bp_iid,
                        3,
                        *runs as i64,
                    );
                }
            }
            let completed_ids: Vec<i64> = completed_jobs.into_iter().map(|(jid, _, _, _, _)| jid).collect();
            Some(Ok(PyRep::keyval([
                ("command", PyRep::String(cmd.to_string())),
                ("characterID", PyRep::Integer(target_cid)),
                ("readyJobCount", PyRep::Integer(completed_ids.len() as i64)),
                (
                    "jobIDs",
                    PyRep::List(completed_ids.into_iter().map(PyRep::Integer).collect()),
                ),
            ])))
        }
        "/cleartimers" => {
            let (target_cid, rem) =
                match resolve_optional_target_char(ctx, caller_cid, &parts[1..], head.as_str()) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
            if let Some(bad_tok) = rem.first() {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: format!("Character '{bad_tok}' not found for /cleartimers"),
                    client_error: None,
                }));
            }
            let now_unix = ctx.now_unix();
            crate::services::space_destiny::clear_session_change_timer(ctx, target_cid);
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&target_cid) {
                    ch.last_clone_jump_unix = 0;
                    ch.jump_fatigue_minutes = 0.0;
                    ch.jump_cooldown_until_unix = 0;
                    ch.jump_fatigue_until_unix = 0;
                }
                snap.item_custom_names.remove(&(-8_800_000 - target_cid));
            });
            {
                let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                if let Some(pst) = cw.states.get_mut(&target_cid) {
                    pst.weapon_timer_until_unix = 0;
                    pst.pvp_timer_until_unix = 0;
                    pst.suspect_flag_until_unix = 0;
                    pst.criminal_flag_until_unix = 0;
                    pst.pending_concord_arrival = None;
                }
            }
            let cleared_engagements: Vec<i64> =
                crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                    rt.npc_timers.remove(&target_cid);
                    rt.disapproval_flags.remove(&target_cid);
                    let mut targets: Vec<i64> = rt
                        .limited_engagements
                        .remove(&target_cid)
                        .map(|m| m.into_keys().collect())
                        .unwrap_or_default();
                    for other_map in rt.limited_engagements.values_mut() {
                        other_map.remove(&target_cid);
                    }
                    targets.sort_unstable();
                    targets
                });
            crate::services::combat_crimewatch::crimewatch_timers::emit_crimewatch_timer_notifications(
                ctx,
                target_cid,
                now_unix,
            );
            for target_id in cleared_engagements {
                ctx.emit_typed(
                    NotificationTarget::Character(target_cid),
                    OnCrimewatchEngagementEnded {
                        other_char_id: target_id,
                    },
                );
            }
            crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                rt.crimewatch_timer_states.remove(&target_cid);
                rt.crimewatch_timer_triggered_at.remove(&target_cid);
            });
            Some(Ok(PyRep::keyval([
                ("command", PyRep::String(cmd.to_string())),
                ("characterID", PyRep::Integer(target_cid)),
                ("cleared", PyRep::Bool(true)),
            ])))
        }
        _ => None,
    }
}

/// Fast-forwards a character's in-space `SystemActor` simulation (`fast_forward_clock`), persists any
/// recharged capacitor/shield attributes to `items[ship_id].dynamic_attributes`, and emits
/// `OnCapacitorChange` / `OnModuleAttributeChanges` so client HUD and dogma remain in sync (#1268).
pub(crate) async fn fast_forward_and_sync_space_clock(
    ctx: &ServerContext,
    target_cid: i64,
    explicit_space: Option<(u32, i64)>,
    secs: u64,
) {
    if secs == 0 {
        return;
    }
    let space = explicit_space.or_else(|| {
        if target_cid <= 0 {
            return None;
        }
        ctx.db.with_snapshot(|snap| {
            let ch = snap.characters.get(&target_cid)?;
            if ch.station_id.is_none()
                && ch.structure_id.is_none()
                && ch.solar_system_id > 0
                && ch.active_ship_id > 0
            {
                Some((ch.solar_system_id, ch.active_ship_id))
            } else {
                None
            }
        })
    });
    let Some((sys_id, ship_id)) = space else {
        return;
    };
    if sys_id == 0 || ship_id <= 0 {
        return;
    }

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let pre_cs = sys_h.get_combat_state(ship_id).await.ok().flatten();
    let _ = sys_h.fast_forward_clock(ship_id, secs).await;
    let Some(post_cs) = sys_h.get_combat_state(ship_id).await.ok().flatten() else {
        return;
    };

    let cap_changed = pre_cs
        .as_ref()
        .is_none_or(|pre| (post_cs.capacitor - pre.capacitor).abs() > 1e-6);
    let shield_changed = pre_cs
        .as_ref()
        .is_none_or(|pre| (post_cs.shield_hp - pre.shield_hp).abs() > 1e-6);

    if cap_changed || shield_changed {
        ctx.db.with_mut_snapshot(|snap| {
            if crate::sp6_lifecycle::is_ship_stale_for_in_space_write_in_snap(
                snap,
                ship_id,
                Some(sys_id),
                None,
            ) {
                return;
            }
            if let Some(item) = snap.items.get_mut(&ship_id) {
                if cap_changed && post_cs.max_capacitor > 0.0 {
                    let clamped_cap = post_cs.capacitor.clamp(0.0, post_cs.max_capacitor);
                    item.dynamic_attributes.insert(18u16, clamped_cap);
                    item.version += 1;
                }
                if shield_changed && post_cs.max_shield_hp > 0.0 {
                    let clamped_shield = post_cs.shield_hp.clamp(0.0, post_cs.max_shield_hp);
                    item.dynamic_attributes
                        .insert(eve_sde::attr_ids::SHIELD_CHARGE, clamped_shield);
                    item.version += 1;
                }
            }
        });
    }

    let notify_cid = if target_cid > 0 {
        target_cid
    } else {
        ctx.db.with_snapshot(|snap| {
            snap.characters
                .values()
                .find(|c| {
                    c.active_ship_id == ship_id
                        && c.solar_system_id == sys_id
                        && c.station_id.is_none()
                        && c.structure_id.is_none()
                })
                .map(|c| c.character_id)
                .unwrap_or(0)
        })
    };
    if notify_cid > 0 {
        if cap_changed && post_cs.max_capacitor > 0.0 {
            let _ = ctx.notify_ship_capacitor_change(
                notify_cid,
                ship_id,
                post_cs.capacitor,
                post_cs.max_capacitor,
                pre_cs.as_ref().map(|c| c.capacitor),
            );
        }
        if shield_changed && post_cs.max_shield_hp > 0.0 {
            let now_ft = ctx.now_system_sim_filetime_ticks_for_ship(notify_cid, ship_id);
            let old_shield = pre_cs
                .as_ref()
                .map(|c| c.shield_hp)
                .unwrap_or(post_cs.shield_hp);
            ctx.emit_typed(
                NotificationTarget::Character(notify_cid),
                crate::router::notify_registry::OnModuleAttributeChanges {
                    changes: PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::String("OnModuleAttributeChange".into()),
                        PyRep::Integer(notify_cid),
                        PyRep::Integer(ship_id),
                        PyRep::Integer(i64::from(eve_sde::attr_ids::SHIELD_CHARGE)),
                        PyRep::Integer(now_ft),
                        PyRep::Real(post_cs.shield_hp),
                        PyRep::Real(old_shield),
                        PyRep::None,
                    ])]),
                },
            );
        }
    }
}

#[cfg(test)]
#[path = "notify_emitters_character_services_tests.rs"]
mod notify_emitters_character_services_tests;


