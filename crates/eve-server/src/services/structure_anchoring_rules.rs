//! `structure_anchoring_rules` — V24.01 Upwell & FLEX anchoring eligibility, spatial clearances,
//! corporate age gating, foreign sovereignty ADM/Strategic-Index rules, and Pochven Customs Office
//! standing checks (`feat:structures.anchor-restrictions`, `feat:structures.pochven-anchoring-restrictions`).
#![forbid(unsafe_code)]

use super::station_structure::structure_lifecycle::{
    self as life, space_class, HullClass, SpaceClass, StructureProfile, ANCHORING_SECS,
    MIN_ANCHOR_DISTANCE_M, PALATINE_KEEPSTAR_TYPE_ID, POCHVEN_REGION_ID, STATE_DESTROYED,
};
use crate::router::ServerContext;
use eve_db::WorldStateSnapshot;
use eve_proto::BridgeRpcError;
use glam::DVec3;

/// Major Empire trade hub solar systems where capsuleer structure anchoring is banned (`Anchoring_structures.md`).
pub const TRADE_HUB_SYSTEM_IDS: [u32; 5] = [
    30_000_142, // Jita
    30_002_187, // Amarr
    30_002_659, // Dodixie
    30_002_510, // Rens
    30_002_053, // Hek
];

/// Minimum distance from deploying ship when explicit anchoring coordinates are supplied (`Anchoring_structures.md`: 50 km).
pub const MIN_ANCHOR_SHIP_DISTANCE_M: f64 = 50_000.0;
/// Maximum distance from deploying ship when explicit anchoring coordinates are supplied (`Anchoring_structures.md`: 75 km).
pub const MAX_ANCHOR_SHIP_DISTANCE_M: f64 = 75_000.0;
/// DERIVED: server fallback offset (`60 km` inside `[50 km, 75 km]`) when 2-arg `Anchor` omits explicit coordinates.
pub const DEFAULT_ANCHOR_SHIP_OFFSET_M: f64 = 60_000.0;
/// Minimum distance from other Upwell structures when anchoring a FLEX structure (`500 km`).
pub const FLEX_MIN_STRUCTURE_DISTANCE_M: f64 = 500_000.0;
/// Minimum distance from stargates when anchoring an Ansiblex Jump Gate (`35841`, `100,000 km`).
pub const ANSIBLEX_MIN_STARGATE_DISTANCE_M: f64 = 100_000_000.0;
/// Base anchoring duration for FLEX structures (`45 minutes = 2,700 s`, V24 `structures/const.pyj` `FLEX_DEPLOYING_TIME = 2700`).
pub const ANSIBLEX_BASE_ANCHORING_SECS: u64 = 45 * 60;
/// Minimum corporation age required before a capsuleer corporation may anchor an Upwell/FLEX structure (`7 days = 604,800 s`, `Anchoring_structures.md`).
pub const MIN_ANCHOR_CORP_AGE_SECS: u64 = 7 * 86_400;
/// Minimum Triglavian Collective (`500026`) standing required to deploy/upgrade a Customs Office in Pochven.
pub const POCHVEN_CUSTOMS_OFFICE_MIN_TRIG_STANDING: f64 = 1.5;

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

pub fn is_trade_hub_system(sys_id: u32) -> bool {
    TRADE_HUB_SYSTEM_IDS.contains(&sys_id)
}

/// Delegates to the runtime SDE-driven starter/career-agent solar system check (`sde_schools` + `sde_agents`).
pub fn is_starter_or_career_system(sde: &eve_sde::SdeRegistry, sys_id: u32) -> bool {
    crate::services::mobile_deployables::is_starter_or_career_system(sde, sys_id)
}

/// Validates spatial, security-band, system-ban, per-system/cluster cap, and celestial/structure
/// clearance rules for anchoring `profile` at `pos` in `sys_id` (`feat:structures.anchor-restrictions`).
pub fn check_anchor_position(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    profile: &StructureProfile,
    sys_id: u32,
    pos: DVec3,
) -> Result<(), BridgeRpcError> {
    if !pos.is_finite() || pos.abs().max_element() > 1.0e14 {
        return Err(err(
            "InvalidAnchorCoordinates",
            "Anchoring position coordinates must be finite and within solar system bounds",
        ));
    }
    let (_, is_thera, is_shattered, _) =
        crate::services::space_destiny::system_wormhole_profile(sys_id);
    if is_thera || is_shattered {
        return Err(err(
            "AnchoringDisallowedInTheraOrShattered",
            format!("Anchoring structures is disallowed in Thera or shattered wormhole system {sys_id}"),
        ));
    }
    if sys_id == 30_100_000 {
        return Err(err(
            "AnchoringDisallowedInZarzakh",
            "Capsuleer structure anchoring is prohibited in Zarzakh",
        ));
    }
    let region_id = ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id).unwrap_or(0);
    if region_id == POCHVEN_REGION_ID {
        return Err(err(
            "AnchoringDisallowedInPochven",
            format!("Upwell structures cannot be anchored in Pochven solar system {sys_id}"),
        ));
    }
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(1.0);
    if profile.hull_class == HullClass::Flex && (sys_sec >= 0.0 || sys_id >= 31_000_000) {
        return Err(err(
            "CannotAnchorFlexOutsideSovNull",
            format!(
                "FLEX structure type {} can only be anchored in sovereign K-space null-security systems (system {sys_id})",
                profile.type_id
            ),
        ));
    }
    if profile.type_id == PALATINE_KEEPSTAR_TYPE_ID
        && snap
            .structures
            .values()
            .any(|s| s.type_id == PALATINE_KEEPSTAR_TYPE_ID && s.state != STATE_DESTROYED)
    {
        return Err(err(
            "PalatineKeepstarLimitReached",
            "Only one Upwell Palatine Keepstar (40340) may be active in the cluster at any time",
        ));
    }
    if profile.hull_class == HullClass::XLarge
        && profile.group_id == 1657
        && snap.structures.values().any(|s| {
            s.solar_system_id == sys_id
                && s.state != STATE_DESTROYED
                && life::structure_profile(ctx, s.type_id)
                    .is_some_and(|p| p.hull_class == HullClass::XLarge && p.group_id == 1657)
        })
    {
        return Err(err(
            "KeepstarPerSystemLimit",
            format!("Only one Keepstar-sized Citadel may be anchored in solar system {sys_id}"),
        ));
    }
    if profile.hull_class == HullClass::Flex
        && snap
            .structures
            .values()
            .any(|s| s.solar_system_id == sys_id && s.state != STATE_DESTROYED && s.type_id == profile.type_id)
    {
        return Err(err(
            "FlexStructurePerSystemLimit",
            format!(
                "Only one FLEX structure of type {} may be anchored in solar system {sys_id}",
                profile.type_id
            ),
        ));
    }
    if profile.disallow_in_highsec && space_class(ctx, sys_id) == SpaceClass::HighSec {
        return Err(err(
            "CannotAnchorInHighSec",
            format!("Structure type {} cannot be anchored in high-security system {sys_id}", profile.type_id),
        ));
    }
    if is_trade_hub_system(sys_id) {
        return Err(err(
            "AnchoringDisallowedInTradeHub",
            format!("Upwell structures cannot be anchored in trade hub solar system {sys_id}"),
        ));
    }
    if is_starter_or_career_system(&ctx.sde, sys_id) {
        return Err(err(
            "AnchoringDisallowedInStarterOrCareerSystem",
            format!("Upwell structures cannot be anchored in starter or career-agent system {sys_id}"),
        ));
    }

    let station_hit = ctx
        .sde
        .stations
        .values()
        .any(|s| s.solar_system_id == sys_id && s.position.distance(pos) < MIN_ANCHOR_DISTANCE_M);
    // Surface clearance (1,000 km) for Planets (7), Moons (8), Asteroid Belts (9), and Stargates (10),
    // plus 100,000 km stargate clearance for Ansiblex Jump Gates (35841).
    let celestial_hit = ctx.sde.celestials_in_system(sys_id).iter().any(|c| match c.group_id {
        7 | 8 | 9 => (c.position.distance(pos) - c.radius).max(0.0) < MIN_ANCHOR_DISTANCE_M,
        10 => {
            let min_gate_dist = if profile.type_id == 35_841 {
                ANSIBLEX_MIN_STARGATE_DISTANCE_M
            } else {
                MIN_ANCHOR_DISTANCE_M
            };
            (c.position.distance(pos) - c.radius).max(0.0) < min_gate_dist
        }
        _ => false,
    });
    let structure_hit = snap.structures.values().any(|s| {
        if s.solar_system_id != sys_id || s.state == STATE_DESTROYED {
            return false;
        }
        let other_is_flex = life::structure_profile(ctx, s.type_id)
            .map(|p| p.hull_class == HullClass::Flex)
            .unwrap_or(false);
        let req_dist = if profile.hull_class == HullClass::Flex && other_is_flex {
            FLEX_MIN_STRUCTURE_DISTANCE_M
        } else {
            MIN_ANCHOR_DISTANCE_M
        };
        DVec3::from_array(s.position).distance(pos) < req_dist
    });
    if station_hit || celestial_hit || structure_hit {
        return Err(err(
            "TooCloseToStructureOrStation",
            format!(
                "Anchoring position violates minimum clearance from stations, celestials, stargates, or structures in system {sys_id}"
            ),
        ));
    }
    Ok(())
}

/// Verifies that `corp_id` belongs to the alliance holding sovereignty in `sys_id` and owning the
/// system's Sovereignty Hub (`32458`) when anchoring a FLEX structure (`feat:structures.anchor-restrictions`).
pub fn check_flex_system_sovereignty(
    snap: &WorldStateSnapshot,
    sys_id: u32,
    corp_id: i64,
    type_id: u32,
) -> Result<(), BridgeRpcError> {
    let caller_alliance = snap.corporations.get(&corp_id).and_then(|c| c.alliance_id);
    let sys_sov_ally = snap.system_states.get(&sys_id).and_then(|s| s.sov_alliance_id);
    let holds_system_sov = caller_alliance.is_some() && caller_alliance == sys_sov_ally;
    let hub_owned_by_caller_alliance = snap.structures.values().any(|s| {
        s.solar_system_id == sys_id
            && s.type_id == crate::services::sov_services::TYPE_SOVEREIGNTY_HUB
            && s.state != STATE_DESTROYED
            && (s.owner_corp_id == corp_id
                || (caller_alliance.is_some()
                    && snap.corporations.get(&s.owner_corp_id).and_then(|c| c.alliance_id)
                        == caller_alliance))
    });
    if !holds_system_sov || !hub_owned_by_caller_alliance {
        return Err(err(
            "FlexAnchoringRequiresSystemSovereignty",
            format!(
                "FLEX structure type {type_id} can only be anchored by a corporation in the alliance holding sovereignty and owning the Sovereignty Hub in system {sys_id}"
            ),
        ));
    }
    Ok(())
}

/// Validates corporate age (>= 7 days), ship-to-anchor distance (`[50 km, 75 km]`, or default `+60 km`),
/// spatial/system anchoring restrictions, FLEX sovereignty/hub ownership, and foreign sovereignty ADM / Strategic Index penalties.
/// Returns `(resolved_anchor_pos, anchoring_duration_secs)`.
#[allow(clippy::too_many_arguments)]
pub fn validate_and_resolve_anchor(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    profile: &StructureProfile,
    sys_id: u32,
    corp_id: i64,
    ship_pos: DVec3,
    explicit_pos: Option<DVec3>,
    now_unix: u64,
) -> Result<(DVec3, u64), BridgeRpcError> {
    if corp_id >= 98_000_000 {
        let corp_created_unix = snap
            .corp_governance
            .get(&corp_id)
            .map(|g| g.created_unix)
            .unwrap_or(0);
        if now_unix.saturating_sub(corp_created_unix) < MIN_ANCHOR_CORP_AGE_SECS {
            return Err(err(
                "CorporationTooYoungToAnchor",
                format!(
                    "Corporation {corp_id} must be at least 7 days old before anchoring an Upwell structure"
                ),
            ));
        }
    }
    let anchor_pos = match explicit_pos {
        Some(pos) => pos,
        None => ship_pos + DVec3::new(DEFAULT_ANCHOR_SHIP_OFFSET_M, 0.0, 0.0),
    };
    if !anchor_pos.is_finite() || anchor_pos.abs().max_element() > 1.0e14 {
        return Err(err(
            "InvalidAnchorCoordinates",
            "Anchoring coordinates must be finite numbers within solar system bounds",
        ));
    }
    let dist = anchor_pos.distance(ship_pos);
    if !(MIN_ANCHOR_SHIP_DISTANCE_M - 1e-3..=MAX_ANCHOR_SHIP_DISTANCE_M + 1e-3).contains(&dist) {
        return Err(err(
            "InvalidAnchorDistanceFromShip",
            format!(
                "Anchoring position must be between 50 km and 75 km from the deploying ship (got {:.1} km)",
                dist / 1_000.0
            ),
        ));
    }

    check_anchor_position(ctx, snap, profile, sys_id, anchor_pos)?;

    if profile.hull_class == HullClass::Flex {
        check_flex_system_sovereignty(snap, sys_id, corp_id, profile.type_id)?;
    }

    let caller_alliance = snap.corporations.get(&corp_id).and_then(|c| c.alliance_id);
    let strat_idx = snap
        .system_states
        .get(&sys_id)
        .map(|s| u64::from(s.strategic_index))
        .unwrap_or(0);
    // V24 base FLEX deploying time is 2,700 s (`FLEX_DEPLOYING_TIME`); DERIVED `(1 + strat_idx)` scaling
    // extends FLEX anchoring duration per Strategic Index level.
    let mut anchoring_duration_secs = if profile.hull_class == HullClass::Flex {
        ANSIBLEX_BASE_ANCHORING_SECS * (1 + strat_idx)
    } else {
        ANCHORING_SECS
    };

    if let Some(sys_state) = snap.system_states.get(&sys_id) {
        if let Some(sov_ally) = sys_state.sov_alliance_id {
            let is_friendly_sov = caller_alliance == Some(sov_ally);
            if !is_friendly_sov {
                let adm = crate::services::sov_services::calculate_adm(
                    sys_state.military_index,
                    sys_state.industrial_index,
                    sys_state.strategic_index,
                );
                if adm > 4.0 && profile.hull_class == HullClass::Medium {
                    return Err(err(
                        "AdmBlocksHostileMediumStructure",
                        format!(
                            "Medium structures cannot be anchored in hostile sovereign system {sys_id} while Activity Defense Multiplier is {:.2} (> 4.0)",
                            adm
                        ),
                    ));
                }
                if profile.hull_class != HullClass::Flex {
                    anchoring_duration_secs = ANCHORING_SECS + strat_idx * 86_400;
                }
            }
        }
    }

    Ok((anchor_pos, anchoring_duration_secs))
}

/// Validates effective Triglavian Collective (`500026`) standing (`>= 1.5`, applying Diplomacy `3357`
/// for negative raw standing and Connections `3359` for non-negative derived standing) when deploying
/// or upgrading a Customs Office / Gantry in Pochven (`feat:structures.pochven-anchoring-restrictions`).
pub fn validate_pochven_customs_office_standing(
    snap: &WorldStateSnapshot,
    char_id: i64,
    sys_id: u32,
    sde: &eve_sde::SdeRegistry,
) -> Result<(), BridgeRpcError> {
    let region_id = sde.solar_systems.get(&sys_id).map(|s| s.region_id).unwrap_or(0);
    if region_id == POCHVEN_REGION_ID {
        let ch_opt = snap.characters.get(&char_id);
        let has_entry = ch_opt.is_some_and(|c| c.standings.contains_key(&500_026));
        let raw = ch_opt.and_then(|c| c.standings.get(&500_026).copied()).unwrap_or(0.0);
        let skill_map = snap.skills.get(&char_id);
        let diplo_lvl = skill_map.and_then(|m| m.get(&3_357)).map(|s| s.level.min(5)).unwrap_or(0);
        let conn_lvl = skill_map.and_then(|m| m.get(&3_359)).map(|s| s.level.min(5)).unwrap_or(0);
        let trig_standing = if raw < 0.0 && diplo_lvl > 0 {
            (raw + (10.0 - raw) * 0.04 * f64::from(diplo_lvl)).clamp(-10.0, 10.0)
        } else if has_entry && raw >= 0.0 && conn_lvl > 0 {
            (raw + (10.0 - raw) * 0.04 * f64::from(conn_lvl)).clamp(-10.0, 10.0)
        } else {
            raw
        };
        if trig_standing < POCHVEN_CUSTOMS_OFFICE_MIN_TRIG_STANDING {
            return Err(err(
                "InsufficientTriglavianStanding",
                format!(
                    "Deploying or upgrading a Customs Office in Pochven requires >= {:.1} effective Triglavian Collective standing (current {:.2})",
                    POCHVEN_CUSTOMS_OFFICE_MIN_TRIG_STANDING, trig_standing
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)] #[path = "structure_anchoring_rules_tests.rs"]
mod structure_anchoring_rules_tests;

