//! Protocol-agnostic corporation governance & role domain helpers (`#593`).

#![forbid(unsafe_code)]

use eve_db::WorldStateSnapshot;
use eve_sde::SdeRegistry;

pub use eve_db::corp_actor::{corp_role_breakdown, CorpRoleBreakdown};
/// Backwards-compatible type alias for [`CorpRoleBreakdown`].
pub type EffectiveCorpRoles = CorpRoleBreakdown;

/// Canonical corporation role definitions `(bitmask, client_role_name, esi_role_name)`
/// shared by MachoNet `cache.Rowset("roles")`, domain checks, and ESI role decoders.
pub use eve_db::corp_roles::CORP_ROLE_NAMES as CORP_ROLE_DEFINITIONS;

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
    eve_db::corp_roles::encode_corp_role_names(names)
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
    let eff = corp_role_breakdown(snap, corp_id, char_id, now_unix);
    eff.is_ceo || (eff.any_active_roles() & ALLOWED_CORP_MAIL_ROLES) != 0
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
