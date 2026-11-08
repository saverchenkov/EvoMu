//! Protocol-agnostic corporation governance & role domain helpers (`#593`).

#![forbid(unsafe_code)]

use eve_db::WorldStateSnapshot;
use eve_sde::SdeRegistry;

/// Canonical corporation role definitions `(bitmask, client_role_name, esi_role_name)`
/// shared by MachoNet `cache.Rowset("roles")`, domain checks, and ESI role decoders.
pub const CORP_ROLE_DEFINITIONS: &[(u64, &str, &str)] = &[
    (eve_db::corp_roles::ROLE_DIRECTOR, "Director", "Director"),
    (
        eve_db::corp_roles::ROLE_DELIVERIES_HANGAR_TAKE,
        "Deliveries Hangar Take",
        "Deliveries_Take",
    ),
    (
        eve_db::corp_roles::ROLE_DELIVERIES_HANGAR_QUERY,
        "Deliveries Hangar Query",
        "Deliveries_Query",
    ),
    (
        eve_db::corp_roles::ROLE_DELIVERIES_HANGAR_CONTAINER_TAKE,
        "Deliveries Hangar Container Take",
        "Deliveries_Container_Take",
    ),
    (
        eve_db::corp_roles::ROLE_PERSONNEL_MANAGER,
        "Personnel Manager",
        "Personnel_Manager",
    ),
    (eve_db::corp_roles::ROLE_ACCOUNTANT, "Accountant", "Accountant"),
    (
        eve_db::corp_roles::ROLE_SECURITY_OFFICER,
        "Security Officer",
        "Security_Officer",
    ),
    (
        eve_db::corp_roles::ROLE_FACTORY_MANAGER,
        "Factory Manager",
        "Factory_Manager",
    ),
    (
        eve_db::corp_roles::ROLE_STATION_MANAGER,
        "Station Manager",
        "Station_Manager",
    ),
    (eve_db::corp_roles::ROLE_AUDITOR, "Auditor", "Auditor"),
    (
        eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[0],
        "Hangar Can Take 1",
        "Hangar_Take_1",
    ),
    (
        eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[1],
        "Hangar Can Take 2",
        "Hangar_Take_2",
    ),
    (
        eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[2],
        "Hangar Can Take 3",
        "Hangar_Take_3",
    ),
    (
        eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[3],
        "Hangar Can Take 4",
        "Hangar_Take_4",
    ),
    (
        eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[4],
        "Hangar Can Take 5",
        "Hangar_Take_5",
    ),
    (
        eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[5],
        "Hangar Can Take 6",
        "Hangar_Take_6",
    ),
    (
        eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[6],
        "Hangar Can Take 7",
        "Hangar_Take_7",
    ),
    (
        eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[0],
        "Hangar Can Query 1",
        "Hangar_Query_1",
    ),
    (
        eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[1],
        "Hangar Can Query 2",
        "Hangar_Query_2",
    ),
    (
        eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[2],
        "Hangar Can Query 3",
        "Hangar_Query_3",
    ),
    (
        eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[3],
        "Hangar Can Query 4",
        "Hangar_Query_4",
    ),
    (
        eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[4],
        "Hangar Can Query 5",
        "Hangar_Query_5",
    ),
    (
        eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[5],
        "Hangar Can Query 6",
        "Hangar_Query_6",
    ),
    (
        eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[6],
        "Hangar Can Query 7",
        "Hangar_Query_7",
    ),
    (
        eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[0],
        "Account Can Take 1",
        "Account_Take_1",
    ),
    (
        eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[1],
        "Account Can Take 2",
        "Account_Take_2",
    ),
    (
        eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[2],
        "Account Can Take 3",
        "Account_Take_3",
    ),
    (
        eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[3],
        "Account Can Take 4",
        "Account_Take_4",
    ),
    (
        eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[4],
        "Account Can Take 5",
        "Account_Take_5",
    ),
    (
        eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[5],
        "Account Can Take 6",
        "Account_Take_6",
    ),
    (
        eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[6],
        "Account Can Take 7",
        "Account_Take_7",
    ),
    (eve_db::corp_roles::ROLE_DIPLOMAT, "Diplomat", "Diplomat"),
    (
        eve_db::corp_roles::ROLE_BRAND_MANAGER,
        "Brand Manager",
        "Brand_Manager",
    ),
    (
        eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG,
        "Equipment Config",
        "Config_Equipment",
    ),
    (
        eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[0],
        "Container Can Take 1",
        "Container_Take_1",
    ),
    (
        eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[1],
        "Container Can Take 2",
        "Container_Take_2",
    ),
    (
        eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[2],
        "Container Can Take 3",
        "Container_Take_3",
    ),
    (
        eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[3],
        "Container Can Take 4",
        "Container_Take_4",
    ),
    (
        eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[4],
        "Container Can Take 5",
        "Container_Take_5",
    ),
    (
        eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[5],
        "Container Can Take 6",
        "Container_Take_6",
    ),
    (
        eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[6],
        "Container Can Take 7",
        "Container_Take_7",
    ),
    (eve_db::corp_roles::ROLE_RENT_OFFICE, "Can Rent Office", "Rent_Office"),
    (
        eve_db::corp_roles::ROLE_RENT_FACTORY_FACILITY,
        "Can Rent Factory Slot",
        "Rent_Factory_Facility",
    ),
    (
        eve_db::corp_roles::ROLE_RENT_RESEARCH_FACILITY,
        "Can Rent Research Slot",
        "Rent_Research_Facility",
    ),
    (
        eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT,
        "Junior Accountant",
        "Junior_Accountant",
    ),
    (
        eve_db::corp_roles::ROLE_STARBASE_CONFIG,
        "Starbase Config",
        "Config_Starbase_Equipment",
    ),
    (eve_db::corp_roles::ROLE_TRADER, "Trader", "Trader"),
    (
        eve_db::corp_roles::ROLE_CHAT_MANAGER,
        "Chat Manager",
        "Communications_Officer",
    ),
    (
        eve_db::corp_roles::ROLE_CONTRACT_MANAGER,
        "Contract Manager",
        "Contract_Manager",
    ),
    (
        eve_db::corp_roles::ROLE_INFRASTRUCTURE_TACTICIAN,
        "Infrastructure Tactical Officer",
        "Starbase_Defense_Operator",
    ),
    (
        eve_db::corp_roles::ROLE_STARBASE_CARETAKER,
        "Starbase Caretaker",
        "Starbase_Fuel_Technician",
    ),
    (
        eve_db::corp_roles::ROLE_FITTING_MANAGER,
        "Fitting Manager",
        "Fitting_Manager",
    ),
    (
        eve_db::corp_roles::ROLE_PROJECT_MANAGER,
        "Project Manager",
        "Project_Manager",
    ),
    (
        eve_db::corp_roles::ROLE_TERRESTRIAL_LOGISTICS_OFFICER,
        "Terrestrial Logistics Officer",
        "",
    ),
    (
        eve_db::corp_roles::ROLE_SKILL_PLAN_MANAGER,
        "Skill Plan Manager",
        "Skill_Plan_Manager",
    ),
];

/// Roles authorized to send corporation or alliance mail when `restrict_corp_mails` is enabled.
pub const ALLOWED_CORP_MAIL_ROLES: u64 = eve_db::corp_roles::ROLE_DIRECTOR
    | eve_db::corp_roles::ROLE_PERSONNEL_MANAGER
    | eve_db::corp_roles::ROLE_CHAT_MANAGER
    | eve_db::corp_roles::ROLE_DIPLOMAT;

pub fn decode_corp_roles(mask: u64) -> Vec<&'static str> {
    CORP_ROLE_DEFINITIONS
        .iter()
        .filter_map(|&(bit, _, esi_name)| {
            ((mask & bit) != 0 && !esi_name.is_empty()).then_some(esi_name)
        })
        .collect()
}

pub fn encode_esi_corp_roles(names: &[String]) -> u64 {
    let mut mask = 0u64;
    for name in names {
        for &(bit, client_name, esi_name) in CORP_ROLE_DEFINITIONS {
            if (!esi_name.is_empty() && name.eq_ignore_ascii_case(esi_name))
                || name.eq_ignore_ascii_case(client_name)
            {
                mask |= bit;
            }
        }
    }
    mask
}

/// Complete effective corporation role breakdown across DB `member_roles`,
/// `corp_role_assignments`, legacy governance titles, ESI `corp_titles`, and 24-hour
/// `pending_role_grants` / `pending_title_role_grants`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EffectiveCorpRoles {
    pub is_ceo: bool,
    pub roles: u64,
    pub roles_at_hq: u64,
    pub roles_at_base: u64,
    pub roles_at_other: u64,
    pub grantable_roles: u64,
    pub grantable_roles_at_hq: u64,
    pub grantable_roles_at_base: u64,
    pub grantable_roles_at_other: u64,
    pub unmatured_mask: u64,
}

impl EffectiveCorpRoles {
    pub fn any_active_roles(&self) -> u64 {
        self.roles | self.roles_at_hq | self.roles_at_base | self.roles_at_other
    }

    pub fn any_grantable_roles(&self) -> u64 {
        self.grantable_roles
            | self.grantable_roles_at_hq
            | self.grantable_roles_at_base
            | self.grantable_roles_at_other
    }
}

pub fn effective_corp_role_breakdown(
    snap: &WorldStateSnapshot,
    corp_id: i64,
    char_id: i64,
    now_unix: u64,
) -> EffectiveCorpRoles {
    effective_corp_role_breakdown_inner(snap, corp_id, char_id, now_unix, true)
}

fn effective_corp_role_breakdown_inner(
    snap: &WorldStateSnapshot,
    corp_id: i64,
    char_id: i64,
    now_unix: u64,
    mask_all_pending_roles: bool,
) -> EffectiveCorpRoles {
    let Some(corp) = snap.corporations.get(&corp_id) else {
        return EffectiveCorpRoles::default();
    };
    let mut out = EffectiveCorpRoles {
        is_ceo: corp.ceo_id == char_id,
        roles: corp.member_roles.get(&char_id).copied().unwrap_or(0),
        ..Default::default()
    };
    if let Some(ra) = snap.corp_role_assignments.get(&(corp_id, char_id)) {
        out.roles |= ra.roles;
        out.roles_at_hq |= ra.roles_at_hq;
        out.roles_at_base |= ra.roles_at_base;
        out.roles_at_other |= ra.roles_at_other;
        out.grantable_roles |= ra.grantable_roles;
        out.grantable_roles_at_hq |= ra.grantable_roles_at_hq;
        out.grantable_roles_at_base |= ra.grantable_roles_at_base;
        out.grantable_roles_at_other |= ra.grantable_roles_at_other;
    }
    let mut unmatured_role_bits = 0u64;
    let mut unmatured_title_bits = 0u64;
    if let Some(gov) = snap.corp_governance.get(&corp_id) {
        if let Some(&(p_mask, matured_at)) = gov.pending_role_grants.get(&char_id) {
            if now_unix >= matured_at {
                out.roles |= p_mask;
            } else if mask_all_pending_roles {
                unmatured_role_bits |= p_mask;
            } else {
                // Only roles subject to 24h stasis (`CORP_DELAYED_DIVISION_ROLE_MASK`, Director,
                // and Container Take bits) are suppressed while pending; `UpdateMember` also packs
                // immediate roles like Accountant/Trader into `pending_role_grants` (#550).
                const DELAYED_STASIS_MASK: u64 = eve_db::corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK
                    | eve_db::corp_roles::ROLE_DIRECTOR
                    | (0x7Fu64 << 42);
                unmatured_role_bits |= p_mask & DELAYED_STASIS_MASK;
            }
        }
        if let Some(&(t_mask, matured_at)) = gov.pending_title_role_grants.get(&char_id) {
            if now_unix >= matured_at {
                out.roles |= t_mask;
            } else {
                unmatured_title_bits |= t_mask;
            }
        }
        if let Some(&tmask) = gov.member_titles.get(&char_id) {
            for (&tid, &(_, troles)) in &gov.titles {
                if (1..=64).contains(&tid) && (tmask & (1u64 << (tid - 1))) != 0 {
                    out.roles |= troles & !unmatured_title_bits;
                }
            }
        }
    }
    if let (Some(tids), Some(titles)) = (
        snap.corp_member_titles.get(&(corp_id, char_id)),
        snap.corp_titles.get(&corp_id),
    ) {
        for t in titles.iter().filter(|t| tids.contains(&t.title_id)) {
            out.roles |= encode_esi_corp_roles(&t.roles) & !unmatured_title_bits;
            out.roles_at_hq |= encode_esi_corp_roles(&t.roles_at_hq) & !unmatured_title_bits;
            out.roles_at_base |= encode_esi_corp_roles(&t.roles_at_base) & !unmatured_title_bits;
            out.roles_at_other |= encode_esi_corp_roles(&t.roles_at_other) & !unmatured_title_bits;
            out.grantable_roles |= encode_esi_corp_roles(&t.grantable_roles);
            out.grantable_roles_at_hq |= encode_esi_corp_roles(&t.grantable_roles_at_hq);
            out.grantable_roles_at_base |= encode_esi_corp_roles(&t.grantable_roles_at_base);
            out.grantable_roles_at_other |= encode_esi_corp_roles(&t.grantable_roles_at_other);
        }
    }
    out.unmatured_mask = unmatured_role_bits | unmatured_title_bits;
    out.roles &= !out.unmatured_mask;
    out.roles_at_hq &= !out.unmatured_mask;
    out.roles_at_base &= !out.unmatured_mask;
    out.roles_at_other &= !out.unmatured_mask;
    out
}

/// Returns `true` if `char_id` is authorized to send corporation mail for `corp_id`
/// (taking `corp_governance.restrict_corp_mails` and effective roles into account).
pub fn can_send_corp_mail(
    snap: &WorldStateSnapshot,
    corp_id: i64,
    char_id: i64,
    now_unix: u64,
) -> bool {
    let restrict = snap
        .corp_governance
        .get(&corp_id)
        .is_some_and(|g| g.restrict_corp_mails);
    if !restrict {
        return true;
    }
    let eff = effective_corp_role_breakdown(snap, corp_id, char_id, now_unix);
    eff.is_ceo || (eff.any_active_roles() & ALLOWED_CORP_MAIL_ROLES) != 0
}

/// Computes `(is_ceo, effective_roles)` for `char_id` in `corp_id` using the unified
/// [`effective_corp_role_breakdown`] rules (including pending 24h role/title delays).
/// Returns `(false, session_roles)` when `corp_id` is not present in `snap.corporations`.
pub fn effective_corp_member_roles(
    snap: &WorldStateSnapshot,
    corp_id: i64,
    char_id: i64,
    session_roles: u64,
    now_unix: u64,
) -> (bool, u64) {
    if !snap.corporations.contains_key(&corp_id) {
        return (false, session_roles);
    }
    let eff = effective_corp_role_breakdown_inner(snap, corp_id, char_id, now_unix, false);
    (eff.is_ceo, eff.roles)
}

/// Computes corporation member limit from a CEO character's trained skills using SDE dogma
/// attribute `191` (`maxCorporationMembersBonus`), defaulting to EVE's base limit of `10`.
pub fn corp_member_limit_from_skills(
    sde: &SdeRegistry,
    snap: &WorldStateSnapshot,
    ceo_id: i64,
) -> i64 {
    let limit = snap
        .skills
        .get(&ceo_id)
        .map(|skills| {
            skills
                .values()
                .map(|s| {
                    let per_lvl = sde
                        .types
                        .get(&s.type_id)
                        .and_then(|t| t.default_attributes.get(&191).copied())
                        .unwrap_or(0.0);
                    (per_lvl.round() as i64) * i64::from(s.level)
                })
                .sum::<i64>()
        })
        .unwrap_or(0);
    if limit <= 0 { 10 } else { limit }
}

/// Resolves the effective member limit for `corp_id`, checking `snap.corp_member_limits`,
/// `snap.corp_governance.member_limit`, and optionally falling back to CEO skill bonuses.
pub fn resolve_corp_member_limit(
    sde: Option<&SdeRegistry>,
    snap: &WorldStateSnapshot,
    corp_id: i64,
) -> Option<i64> {
    let corp = snap.corporations.get(&corp_id)?;
    if let Some(&stored) = snap.corp_member_limits.get(&corp_id) {
        return Some(stored);
    }
    if let Some(gov_limit) = snap
        .corp_governance
        .get(&corp_id)
        .and_then(|g| g.member_limit.map(|l| l as i64))
    {
        return Some(gov_limit);
    }
    Some(match sde {
        Some(reg) => corp_member_limit_from_skills(reg, snap, corp.ceo_id),
        None => 10,
    })
}
