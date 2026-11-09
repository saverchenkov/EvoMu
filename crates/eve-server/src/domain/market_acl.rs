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
pub fn is_public_available_contract(c: &ContractRecord) -> bool {
    (c.availability.is_empty() || c.availability.eq_ignore_ascii_case("Public"))
        && c.assignee_id == 0
}

/// Returns `true` if `char_id` (with `corp_id` and `alliance_id`) may view contract `c`.
pub fn is_contract_visible_to_character(
    c: &ContractRecord,
    char_id: i64,
    corp_id: i64,
    alliance_id: Option<i64>,
) -> bool {
    if c.issuer_id == char_id {
        return true;
    }
    if is_public_available_contract(c) {
        return true;
    }
    if c.assignee_id != 0 {
        return c.assignee_id == char_id
            || (corp_id != 0 && c.assignee_id == corp_id)
            || alliance_id.is_some_and(|a| a != 0 && a == c.assignee_id);
    }
    let avail = if c.availability.is_empty() {
        "Public"
    } else {
        c.availability.as_str()
    };
    match avail {
        "Corporation" => c.issuer_corp_id != 0 && c.issuer_corp_id == corp_id,
        "Alliance" => alliance_id.is_some() && alliance_id == Some(c.assignee_id),
        "Personal" | "Private" => false,
        _ => true,
    }
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
        (ch.corporation_id != 0
            && (c.assignee_id == ch.corporation_id
                || (c.availability.eq_ignore_ascii_case("Corporation")
                    && c.issuer_corp_id == ch.corporation_id)))
            || ch.alliance_id.is_some_and(|a| a != 0 && c.assignee_id == a)
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
