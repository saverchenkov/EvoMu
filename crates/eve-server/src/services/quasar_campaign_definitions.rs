//! `quasar_campaign_definitions` — V24.01 Quasar Campaigns, Objectives, Tracks & Definitions
//! (`feat:quasar.campaign`, `quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/api/requests_pb2.pyj`,
//!  `quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/api/requests_pb2.pyj`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::PublicGatewayRouter;
use super::quasar_progression_extra::ProgressionExtraManager;
pub use super::quasar_campaign_handlers::extract_campaign_id;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, GoogleAny,
    TYPE_GOOGLEAPIS_PREFIX,
};
use std::sync::Arc;

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

/// Registers campaign definition handlers on the public router.
pub fn register_campaign_definition_handlers(
    router: &PublicGatewayRouter,
    manager: Arc<ProgressionExtraManager>,
) {
    // 1. eve_public.campaign.contributionmethod.definition.api.GetAllRequest
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

    // 2. eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest
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
