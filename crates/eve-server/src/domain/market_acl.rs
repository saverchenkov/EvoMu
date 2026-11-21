//! Protocol-agnostic Structure ACL, Market Visibility & Contract Visibility domain helpers (`#593`).

#![forbid(unsafe_code)]

use crate::domain::DomainError;
use crate::router::ServerContext;
use crate::services::market_trading::is_structure_market_online;
use crate::services::station_structure::structure_upwell_ops::{
    evaluate_character_structure_access_in_snap_at,
    structure_allows_market_orders,
};
use eve_db::{ContractRecord, UpwellStructureRecord, WorldStateSnapshot};
use eve_sde::SdeRegistry;

pub const INDUSTRY_FACILITY_ACTIVITIES: [u8; 6] = [1, 3, 4, 5, 8, 9];

/// Evaluates whether `char_id` is authorized by corp/alliance/standing/war/deny ACL rules to
/// view or dock at `st`.
pub fn character_has_structure_acl_access(
    snap: &WorldStateSnapshot,
    char_id: i64,
    st: &UpwellStructureRecord,
) -> bool {
    if st.state == "Destroyed" {
        return false;
    }
    if snap
        .structure_docking_acl_denied
        .get(&st.structure_id)
        .is_some_and(|set| set.contains(&char_id))
    {
        return false;
    }
    let Some(ch) = snap.characters.get(&char_id) else {
        return false;
    };
    if ch.corporation_id == st.owner_corp_id {
        return true;
    }
    if ch.alliance_id.is_some()
        && snap
            .corporations
            .get(&st.owner_corp_id)
            .and_then(|c| c.alliance_id)
            == ch.alliance_id
    {
        return true;
    }
    let standing = ch.standings.get(&st.owner_corp_id).copied().unwrap_or(0.0);
    if standing < 0.0 {
        return false;
    }
    let at_war = snap.wars.values().any(|w| {
        w.active
            && ((w.aggressor_corp_id == ch.corporation_id && w.defender_corp_id == st.owner_corp_id)
                || (w.aggressor_corp_id == st.owner_corp_id
                    && w.defender_corp_id == ch.corporation_id))
    });
    !at_war
}

/// Evaluates both basic Upwell ACL rules and full structure access profile rules
/// (`structure_upwell_ops`) for `caller_char_id` at `location_id`.
pub fn can_caller_view_structure_acl(
    snap: &WorldStateSnapshot,
    caller_char_id: i64,
    location_id: i64,
    now_unix_opt: Option<u64>,
) -> bool {
    let Some(st) = snap.structures.get(&location_id) else {
        return true;
    };
    if st.state == "Destroyed" {
        return false;
    }
    if caller_char_id <= 0 {
        return evaluate_character_structure_access_in_snap_at(snap, location_id, 0, now_unix_opt);
    }
    character_has_structure_acl_access(snap, caller_char_id, st)
        && evaluate_character_structure_access_in_snap_at(
            snap,
            location_id,
            caller_char_id,
            now_unix_opt,
        )
}

/// Returns `true` if `location_id` is an NPC station (not in `snap.structures`) OR is an Upwell
/// structure whose market hub module is online and accessible to `caller_char_id` at `now_unix_opt`.
pub fn can_caller_view_market_location_at(
    ctx: Option<&ServerContext>,
    snap: &WorldStateSnapshot,
    caller_char_id: i64,
    location_id: i64,
    now_unix_opt: Option<u64>,
) -> bool {
    let Some(st) = snap.structures.get(&location_id) else {
        return true;
    };
    if st.state == "Destroyed" || !is_structure_market_online(snap, location_id) {
        return false;
    }
    if let Some(c) = ctx {
        if !structure_allows_market_orders(c, snap, location_id) {
            return false;
        }
    }
    if caller_char_id <= 0 {
        return evaluate_character_structure_access_in_snap_at(snap, location_id, 0, now_unix_opt);
    }
    if !character_has_structure_acl_access(snap, caller_char_id, st)
        || !evaluate_character_structure_access_in_snap_at(
            snap,
            location_id,
            caller_char_id,
            now_unix_opt,
        )
    {
        return false;
    }
    if crate::services::structure_profile_services::require_structure_service_access_in_snap(
        ctx,
        snap,
        location_id,
        caller_char_id.max(0),
        crate::services::structure_profile_services::StructureServiceKind::Market,
        now_unix_opt.unwrap_or(0),
    )
    .is_err()
    {
        return false;
    }
    if let (Some(c), Some(now_unix)) = (ctx, now_unix_opt) {
        return crate::services::structure_profile_services::check_upwell_market_access_and_rate(
            c,
            snap,
            location_id,
            caller_char_id,
            now_unix,
            "MarketHubOffline",
        )
        .is_ok();
    }
    true
}

/// Returns `true` if `location_id` is an NPC station (not in `snap.structures`) OR is an Upwell
/// structure whose market hub module is online and accessible to `caller_char_id`.
pub fn can_caller_view_market_location(
    snap: &WorldStateSnapshot,
    caller_char_id: i64,
    location_id: i64,
) -> bool {
    can_caller_view_market_location_at(None, snap, caller_char_id, location_id, None)
}

/// Validates direct market-order queries against a specific Upwell `structure_id` (used by ESI
/// `GET /markets/structures/{structure_id}` and Quasar `GetOpenInStructureRequest`).
pub fn verify_structure_market_query_access(
    ctx: Option<&ServerContext>,
    snap: &WorldStateSnapshot,
    caller_char_id: i64,
    structure_id: i64,
    require_structure_exists: bool,
) -> Result<(), DomainError> {
    let Some(st) = snap.structures.get(&structure_id) else {
        if require_structure_exists {
            return Err(DomainError::not_found(
                "StructureNotFound",
                format!("Structure {structure_id} not found"),
            ));
        }
        return Ok(());
    };
    let allows_market = match ctx {
        Some(c) => structure_allows_market_orders(c, snap, structure_id),
        None => true,
    };
    if st.state == "Destroyed" || !is_structure_market_online(snap, structure_id) || !allows_market {
        return Err(DomainError::forbidden(
            "StructureServiceAccessDenied",
            format!("Structure {structure_id} does not have an online Standup Market Hub service"),
        ));
    }
    if !can_caller_view_market_location_at(
        ctx,
        snap,
        caller_char_id,
        structure_id,
        ctx.map(|c| c.now_unix()),
    ) {
        return Err(DomainError::forbidden(
            "StructureServiceAccessDenied",
            format!("Character {caller_char_id} lacks ACL access to structure {structure_id}"),
        ));
    }
    Ok(())
}

/// Returns the first online industry/reaction activity ID for a non-destroyed Upwell structure.
pub fn primary_online_industry_activity(snap: &WorldStateSnapshot, st: &UpwellStructureRecord) -> Option<u8> {
    if st.state == "Destroyed" {
        return None;
    }
    INDUSTRY_FACILITY_ACTIVITIES.into_iter().find(|&act| {
        crate::services::industry_pi::industry_facilities_bps::is_structure_industry_service_online(
            snap,
            st.structure_id,
            act,
        )
    })
}

/// Returns `true` if `c` is a publicly listed unassigned contract.
/// Closed canonical set of contract availability scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractAvailability {
    Public,
    Personal,
    Private,
    Corporation,
    Alliance,
}

impl ContractAvailability {
    /// Parses a wire/record availability string case-insensitively.
    /// Empty strings default to `Public` for legacy records; unknown values return `None`.
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Some(Self::Public);
        }
        match trimmed.to_ascii_lowercase().as_str() {
            "public" => Some(Self::Public),
            "personal" => Some(Self::Personal),
            "private" | "myself" => Some(Self::Private),
            "corporation" | "mycorp" | "my_corp" => Some(Self::Corporation),
            "alliance" | "myalliance" | "my_alliance" => Some(Self::Alliance),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "Public",
            Self::Personal => "Personal",
            Self::Private => "Private",
            Self::Corporation => "Corporation",
            Self::Alliance => "Alliance",
        }
    }

    pub const fn wire_code(self) -> i64 {
        match self {
            Self::Public => 0,
            Self::Personal | Self::Private => 1,
            Self::Corporation => 2,
            Self::Alliance => 3,
        }
    }
}

/// Resolves the alliance ID of a contract's issuer (preferring issuer corp, falling back to issuer char).
pub fn contract_issuer_alliance_id(snap: &WorldStateSnapshot, c: &ContractRecord) -> Option<i64> {
    if c.issuer_corp_id != 0 {
        if let Some(aid) = snap
            .corporations
            .get(&c.issuer_corp_id)
            .and_then(|corp| corp.alliance_id)
            .filter(|&a| a != 0)
        {
            return Some(aid);
        }
    }
    snap.characters
        .get(&c.issuer_id)
        .and_then(|ch| ch.alliance_id)
        .filter(|&a| a != 0)
}

pub fn is_public_available_contract(c: &ContractRecord) -> bool {
    ContractAvailability::parse(&c.availability) == Some(ContractAvailability::Public)
        && c.assignee_id == 0
}

/// Returns `true` if `char_id` (with `corp_id` and `alliance_id`) may view contract `c`.
/// Fails closed on unknown availability scopes and unassigned `Alliance` contracts (use
/// [`is_contract_visible_to_character_in_snap`] when snapshot state is available).
pub fn is_contract_visible_to_character(
    c: &ContractRecord,
    char_id: i64,
    corp_id: i64,
    alliance_id: Option<i64>,
) -> bool {
    if c.issuer_id == char_id {
        return true;
    }
    let Some(avail) = ContractAvailability::parse(&c.availability) else {
        return false;
    };
    if avail == ContractAvailability::Public && c.assignee_id == 0 {
        return true;
    }
    if c.assignee_id != 0 {
        return c.assignee_id == char_id
            || (corp_id != 0 && c.assignee_id == corp_id)
            || alliance_id.is_some_and(|a| a != 0 && a == c.assignee_id);
    }
    match avail {
        ContractAvailability::Corporation => c.issuer_corp_id != 0 && c.issuer_corp_id == corp_id,
        ContractAvailability::Alliance
        | ContractAvailability::Public
        | ContractAvailability::Personal
        | ContractAvailability::Private => false,
    }
}

fn resolve_corp_and_alliance_in_snap(
    snap: &WorldStateSnapshot,
    char_id: i64,
    fallback_corp_id: i64,
    fallback_alliance_id: Option<i64>,
) -> (i64, Option<i64>) {
    if let Some(ch) = snap.characters.get(&char_id) {
        let eff_corp = if ch.corporation_id != 0 {
            ch.corporation_id
        } else {
            fallback_corp_id
        };
        let eff_alliance = if let Some(corp) = (eff_corp != 0)
            .then(|| snap.corporations.get(&eff_corp))
            .flatten()
        {
            corp.alliance_id.filter(|&a| a != 0)
        } else {
            ch.alliance_id.filter(|&a| a != 0)
        };
        return (eff_corp, eff_alliance);
    }
    let eff_alliance = if let Some(corp) = (fallback_corp_id != 0)
        .then(|| snap.corporations.get(&fallback_corp_id))
        .flatten()
    {
        corp.alliance_id.filter(|&a| a != 0)
    } else {
        fallback_alliance_id.filter(|&a| a != 0)
    };
    (fallback_corp_id, eff_alliance)
}

/// Snapshot-aware variant of [`is_contract_visible_to_character`] that resolves corp/alliance
/// authoritatively from `snap` (falling back to caller parameters only when absent from `snap`),
/// including legacy unassigned (`assignee_id == 0`) `"Alliance"` contracts.
pub fn is_contract_visible_to_character_in_snap(
    snap: &WorldStateSnapshot,
    c: &ContractRecord,
    char_id: i64,
    corp_id: i64,
    alliance_id: Option<i64>,
) -> bool {
    let (eff_corp, eff_alliance) =
        resolve_corp_and_alliance_in_snap(snap, char_id, corp_id, alliance_id);
    if is_contract_visible_to_character(c, char_id, eff_corp, eff_alliance) {
        return true;
    }
    if ContractAvailability::parse(&c.availability) == Some(ContractAvailability::Alliance)
        && c.assignee_id == 0
    {
        return eff_alliance
            .is_some_and(|caller_a| contract_issuer_alliance_id(snap, c) == Some(caller_a));
    }
    false
}

/// Returns `true` if `char_id` is an issuer, assignee, acceptor, bidder, or matching corp/alliance
/// member on contract `c`.
pub fn is_character_party_to_contract(
    snap: &WorldStateSnapshot,
    char_id: i64,
    c: &ContractRecord,
) -> bool {
    if (c.issuer_id == char_id && !c.for_corp)
        || c.acceptor_id == Some(char_id)
        || c.assignee_id == char_id
        || c.current_bidder_id == Some(char_id)
        || c.bid_history.iter().any(|&(b, _, _)| b == char_id)
    {
        return true;
    }
    snap.characters.get(&char_id).is_some_and(|ch| {
        let (_, ch_alliance) = resolve_corp_and_alliance_in_snap(snap, char_id, 0, None);
        (ch.corporation_id != 0
            && (c.assignee_id == ch.corporation_id
                || (ContractAvailability::parse(&c.availability)
                    == Some(ContractAvailability::Corporation)
                    && c.issuer_corp_id == ch.corporation_id)))
            || ch_alliance.is_some_and(|a| {
                c.assignee_id == a
                    || (ContractAvailability::parse(&c.availability)
                        == Some(ContractAvailability::Alliance)
                        && c.assignee_id == 0
                        && contract_issuer_alliance_id(snap, c) == Some(a))
            })
    })
}

/// Resolves whether a contract starting at `start_location_id` belongs to `reg_u32` (across both
/// NPC stations and Upwell structures).
pub fn contract_in_region(
    sde: &SdeRegistry,
    snap: &WorldStateSnapshot,
    start_location_id: i64,
    reg_u32: u32,
) -> bool {
    if let Some(st) = sde.stations.get(&start_location_id) {
        return st.region_id == reg_u32;
    }
    if let Some(upwell) = snap.structures.get(&start_location_id) {
        return sde
            .solar_systems
            .get(&upwell.solar_system_id)
            .is_some_and(|sys| sys.region_id == reg_u32);
    }
    false
}
