//! Legal-fixture verification for test setup states (`#1540`).
//!
//! Runs at the end of scenario setup (and in negative fixture unit/integration tests) to ensure a
//! fixture represents a reachable, legal EVE game state before a test exercises RPCs:
//! - authenticated logged-in session tied to a valid non-banned account & owned character
//! - valid NPC starter corp (with zero player corp roles) or existing player corp + roles
//! - active ship exists, is owned by the character, and matches the session's docked/space state
//! - trained skills satisfy ship & fitted module prerequisites
//! - every loaded charge matches the fitted module's accepted group & charge size

#![forbid(unsafe_code)]

use eve_db::{
    corp_roles, CharacterRecord, CorporationRecord, InventoryFlag, InventoryItemRecord,
    WorldStateSnapshot,
};
use eve_sde::{InvType, SdeRegistry, TypeId};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

/// Session attributes inspected by [`check_legal_fixture`], decoupled from `eve-machonet` so
/// `eve-testkit` stays cycle-free.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FixtureSessionView {
    pub session_id: u64,
    pub user_id: Option<u64>,
    pub character_id: Option<i64>,
    pub corporation_id: Option<i64>,
    pub alliance_id: Option<i64>,
    pub station_id: Option<i64>,
    pub structure_id: Option<i64>,
    pub solar_system_id: Option<u32>,
    pub ship_id: Option<i64>,
    pub corp_role: u64,
    pub fleet_id: Option<i64>,
    pub role: u64,
    pub now_unix: Option<u64>,
}

/// Caller expectations enforced on top of baseline legal-fixture consistency.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LegalFixtureExpectation {
    /// Require the character to belong to a real non-NPC player corporation in `snap.corporations`.
    pub require_player_corp: bool,
    /// Required corporation role bitmask (checked via `corp_roles::has_role`).
    pub expected_corp_roles: u64,
    /// When `Some(loc_id)`, require docked location (`station_id` or `structure_id`) to equal `loc_id`.
    pub expected_dock_location_id: Option<i64>,
    /// `Some(true)` = must be undocked in space; `Some(false)` = must be docked; `None` = verify consistency.
    pub expect_in_space: Option<bool>,
    /// Optional runtime ballpark presence check supplied by live server tests (`Some(false)` fails).
    pub ship_in_space_ballpark: Option<bool>,
    /// Require the actor to be an active member of `session.fleet_id` in `snap.fleets`.
    pub require_fleet_membership: bool,
    /// Require the character to currently hold Omega clone grade (`clone_grade >= 2`).
    pub expect_omega: bool,
}

/// Specific invariant violation detected by [`check_legal_fixture`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegalFixtureViolation {
    UnauthenticatedSession { session_id: u64 },
    MissingOrBannedAccount { user_id: u64 },
    MissingCharacter { character_id: i64 },
    CharacterAccountMismatch { character_id: i64, session_user_id: u64, owner_user_id: u64 },
    NonOmegaCharacter { character_id: i64, clone_grade: u32 },
    MissingCorporationId { character_id: i64 },
    CharacterCorporationMismatch { character_id: i64, session_corp_id: i64, character_corp_id: i64 },
    NpcCorpUsedAsPlayerCorp { character_id: i64, corporation_id: i64 },
    NpcCorpCannotGrantPlayerRoles { character_id: i64, corporation_id: i64, corp_role: u64 },
    MissingCorporation { corporation_id: i64 },
    CharacterNotCorpMember { character_id: i64, corporation_id: i64 },
    MissingCorpRoles { character_id: i64, corporation_id: i64, expected: u64, actual: u64 },
    PhantomSessionCorpRoles { character_id: i64, corporation_id: i64, session_roles: u64, actual_roles: u64 },
    InvalidSessionLocation { character_id: i64, reason: String },
    ExpectedInSpaceButDocked { character_id: i64, dock_location_id: i64 },
    ExpectedDockedButInSpace { character_id: i64, solar_system_id: u32 },
    DockLocationMismatch { character_id: i64, expected: i64, actual: i64 },
    MissingActiveShip { character_id: i64, ship_id: Option<i64> },
    InvalidActiveShip { character_id: i64, ship_id: i64, reason: String },
    ShipLocationMismatch { ship_id: i64, expected_location_id: i64, actual_location_id: i64, expected_flag: InventoryFlag, actual_flag: InventoryFlag },
    ShipMissingFromSpaceBallpark { ship_id: i64, solar_system_id: u32 },
    UnownedFittedItem { ship_id: i64, item_id: i64, type_id: TypeId, expected_owner_id: i64, actual_owner_id: i64 },
    DuplicateSlotModule { ship_id: i64, slot_flag: u16, first_item_id: i64, duplicate_item_id: i64, type_id: TypeId },
    InvalidFittedModule { ship_id: i64, item_id: i64, type_id: TypeId, reason: String },
    MissingRequiredSkill { character_id: i64, for_item_id: i64, for_type_id: TypeId, skill_type_id: TypeId, required_level: u8, actual_level: u8 },
    OrphanLoadedCharge { ship_id: i64, charge_item_id: i64, charge_type_id: TypeId, slot_flag: u16 },
    IncompatibleLoadedCharge { ship_id: i64, slot_flag: u16, module_item_id: i64, module_type_id: TypeId, charge_item_id: i64, charge_type_id: TypeId },
    InvalidLoadedChargeQuantity { ship_id: i64, slot_flag: u16, charge_item_id: i64, charge_type_id: TypeId, quantity: i32 },
    ChargeExceedsModuleCapacity { ship_id: i64, slot_flag: u16, module_item_id: i64, module_type_id: TypeId, charge_item_id: i64, charge_type_id: TypeId, quantity: i32, total_volume_milli: i64, module_capacity_milli: i64 },
    FleetMembershipMismatch { character_id: i64, fleet_id: Option<i64>, reason: String },
}

impl fmt::Display for LegalFixtureViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Returns `true` when `corp_id` belongs to an NPC corporation (`1_000_000..=2_000_000` or SDE NPC corp).
#[inline]
pub fn is_npc_corporation(corp_id: i64, sde: &SdeRegistry) -> bool {
    (1_000_000..=2_000_000).contains(&corp_id) || sde.npc_corporations.contains_key(&corp_id)
}

/// Returns `true` iff `charge_type` is compatible with `weapon_type` under the server's dogma
/// `chargeGroup1..5` (`604..=610`), `launcherGroup` (`137`), mining crystal, and `chargeSize` (`128`) rules.
pub fn is_charge_compatible_with_module(weapon_type: &InvType, charge_type: &InvType) -> bool {
    if charge_type.category_id != 8 {
        return false;
    }
    let mining_crystal_ok =
        weapon_type.type_id == 28_748 && matches!(charge_type.group_id, 482 | 663);
    let group_ok = mining_crystal_ok
        || [604_u16, 605, 606, 609, 610].iter().any(|attr_id| {
            weapon_type
                .default_attributes
                .get(attr_id)
                .map(|g| (*g as u32) == charge_type.group_id)
                .unwrap_or(false)
        })
        || (![604_u16, 605, 606, 609, 610]
            .iter()
            .any(|attr_id| weapon_type.default_attributes.contains_key(attr_id))
            && charge_type
                .default_attributes
                .get(&137)
                .map(|lg| *lg as u32 == weapon_type.group_id)
                .unwrap_or(false));
    let size_ok = match (
        weapon_type.default_attributes.get(&128).copied(),
        charge_type.default_attributes.get(&128).copied(),
    ) {
        (Some(rs), Some(cs)) => (rs - cs).abs() < 0.5,
        _ => true,
    };
    group_ok && size_ok
}

#[inline]
fn is_module_rack_flag(flag_u16: u16) -> bool {
    (11..=34).contains(&flag_u16)
}

#[inline]
fn is_fitted_slot_flag(flag_u16: u16) -> bool {
    is_module_rack_flag(flag_u16) || (92..=94).contains(&flag_u16) || (125..=129).contains(&flag_u16)
}

fn collect_required_skills_recursive(
    sde: &SdeRegistry,
    type_id: TypeId,
    out: &mut BTreeMap<TypeId, u8>,
    visited: &mut HashSet<TypeId>,
    depth: u8,
) {
    if depth > 8 || !visited.insert(type_id) {
        return;
    }
    for (skill_id, req_level) in sde.required_skills_for_type(type_id) {
        if req_level == 0 {
            continue;
        }
        out.entry(skill_id)
            .and_modify(|lvl| *lvl = (*lvl).max(req_level))
            .or_insert(req_level);
        if sde.types.contains_key(&skill_id) {
            collect_required_skills_recursive(sde, skill_id, out, visited, depth + 1);
        }
    }
}

fn effective_corp_roles_for_member(corp: &CorporationRecord, char_id: i64) -> u64 {
    if corp.ceo_id == char_id {
        u64::MAX
    } else {
        corp.member_base_roles(char_id)
    }
}

fn validate_corporation_and_roles(
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    session: &FixtureSessionView,
    ch: &CharacterRecord,
    char_id: i64,
    expectation: &LegalFixtureExpectation,
    violations: &mut Vec<LegalFixtureViolation>,
) {
    let Some(sess_corp_id) = session.corporation_id else {
        violations.push(LegalFixtureViolation::MissingCorporationId {
            character_id: char_id,
        });
        return;
    };
    if sess_corp_id != ch.corporation_id {
        violations.push(LegalFixtureViolation::CharacterCorporationMismatch {
            character_id: char_id,
            session_corp_id: sess_corp_id,
            character_corp_id: ch.corporation_id,
        });
    }
    if is_npc_corporation(sess_corp_id, sde) {
        if expectation.require_player_corp {
            violations.push(LegalFixtureViolation::NpcCorpUsedAsPlayerCorp {
                character_id: char_id,
                corporation_id: sess_corp_id,
            });
        }
        if expectation.expected_corp_roles != 0 || session.corp_role != 0 {
            violations.push(LegalFixtureViolation::NpcCorpCannotGrantPlayerRoles {
                character_id: char_id,
                corporation_id: sess_corp_id,
                corp_role: session.corp_role | expectation.expected_corp_roles,
            });
        }
        if let Some(corp) = snap.corporations.get(&sess_corp_id) {
            if corp.ceo_id == char_id {
                violations.push(LegalFixtureViolation::NpcCorpCannotGrantPlayerRoles {
                    character_id: char_id,
                    corporation_id: sess_corp_id,
                    corp_role: corp_roles::ROLE_DIRECTOR,
                });
            }
        }
        return;
    }

    let Some(corp) = snap.corporations.get(&sess_corp_id) else {
        violations.push(LegalFixtureViolation::MissingCorporation {
            corporation_id: sess_corp_id,
        });
        return;
    };
    if corp.ceo_id != char_id && !corp.members.contains(&char_id) {
        violations.push(LegalFixtureViolation::CharacterNotCorpMember {
            character_id: char_id,
            corporation_id: sess_corp_id,
        });
    }
    let actual_roles = effective_corp_roles_for_member(corp, char_id);
    if expectation.expected_corp_roles != 0
        && !corp_roles::has_role(actual_roles, expectation.expected_corp_roles)
    {
        violations.push(LegalFixtureViolation::MissingCorpRoles {
            character_id: char_id,
            corporation_id: sess_corp_id,
            expected: expectation.expected_corp_roles,
            actual: actual_roles,
        });
    }
    if (session.corp_role & !actual_roles) != 0 {
        violations.push(LegalFixtureViolation::PhantomSessionCorpRoles {
            character_id: char_id,
            corporation_id: sess_corp_id,
            session_roles: session.corp_role,
            actual_roles,
        });
    }
}

fn validate_location_and_ship(
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    session: &FixtureSessionView,
    ch: &CharacterRecord,
    char_id: i64,
    expectation: &LegalFixtureExpectation,
    violations: &mut Vec<LegalFixtureViolation>,
) -> Option<(i64, InvType)> {
    if session.station_id.is_some() && session.structure_id.is_some() {
        violations.push(LegalFixtureViolation::InvalidSessionLocation {
            character_id: char_id,
            reason: "session sets both station_id and structure_id".to_string(),
        });
    }
    let Some(sys_id) = session.solar_system_id else {
        violations.push(LegalFixtureViolation::InvalidSessionLocation {
            character_id: char_id,
            reason: "session.solar_system_id is None".to_string(),
        });
        return None;
    };
    if sys_id != ch.solar_system_id {
        violations.push(LegalFixtureViolation::InvalidSessionLocation {
            character_id: char_id,
            reason: format!(
                "session solar_system_id {sys_id} != character solar_system_id {}",
                ch.solar_system_id
            ),
        });
    }
    if session.station_id != ch.station_id || session.structure_id != ch.structure_id {
        violations.push(LegalFixtureViolation::InvalidSessionLocation {
            character_id: char_id,
            reason: format!(
                "session dock ({:?}, {:?}) != character dock ({:?}, {:?})",
                session.station_id, session.structure_id, ch.station_id, ch.structure_id
            ),
        });
    }

    let dock_id = session.station_id.or(session.structure_id);
    match (expectation.expect_in_space, dock_id) {
        (Some(true), Some(dock)) => violations.push(LegalFixtureViolation::ExpectedInSpaceButDocked {
            character_id: char_id,
            dock_location_id: dock,
        }),
        (Some(false), None) => violations.push(LegalFixtureViolation::ExpectedDockedButInSpace {
            character_id: char_id,
            solar_system_id: sys_id,
        }),
        _ => {}
    }
    if let (Some(exp_dock), Some(actual_dock)) = (expectation.expected_dock_location_id, dock_id) {
        if exp_dock != actual_dock {
            violations.push(LegalFixtureViolation::DockLocationMismatch {
                character_id: char_id,
                expected: exp_dock,
                actual: actual_dock,
            });
        }
    }

    let Some(ship_id) = session.ship_id else {
        violations.push(LegalFixtureViolation::MissingActiveShip {
            character_id: char_id,
            ship_id: None,
        });
        return None;
    };
    if ch.active_ship_id > 0 && ch.active_ship_id != ship_id {
        violations.push(LegalFixtureViolation::InvalidActiveShip {
            character_id: char_id,
            ship_id,
            reason: format!(
                "session ship_id {ship_id} != character active_ship_id {}",
                ch.active_ship_id
            ),
        });
    }
    let Some(ship) = snap.items.get(&ship_id) else {
        violations.push(LegalFixtureViolation::MissingActiveShip {
            character_id: char_id,
            ship_id: Some(ship_id),
        });
        return None;
    };
    if ship.owner_id != char_id {
        violations.push(LegalFixtureViolation::InvalidActiveShip {
            character_id: char_id,
            ship_id,
            reason: format!("ship owner_id {} != character_id {char_id}", ship.owner_id),
        });
    }
    if !ship.singleton {
        violations.push(LegalFixtureViolation::InvalidActiveShip {
            character_id: char_id,
            ship_id,
            reason: "active ship item must be singleton".to_string(),
        });
    }
    let Some(ship_type) = sde.types.get(&ship.type_id).cloned() else {
        violations.push(LegalFixtureViolation::InvalidActiveShip {
            character_id: char_id,
            ship_id,
            reason: format!("unknown SDE ship type_id {}", ship.type_id),
        });
        return None;
    };
    if ship_type.category_id != 6 {
        violations.push(LegalFixtureViolation::InvalidActiveShip {
            character_id: char_id,
            ship_id,
            reason: format!(
                "type_id {} has category_id {}, expected Ship (6)",
                ship.type_id, ship_type.category_id
            ),
        });
    }

    let (expected_loc, expected_flag) = match dock_id {
        Some(dock) => (dock, InventoryFlag::Hangar),
        None => (i64::from(sys_id), InventoryFlag::None),
    };
    if ship.location_id != expected_loc || ship.flag != expected_flag {
        violations.push(LegalFixtureViolation::ShipLocationMismatch {
            ship_id,
            expected_location_id: expected_loc,
            actual_location_id: ship.location_id,
            expected_flag,
            actual_flag: ship.flag,
        });
    }
    if dock_id.is_none() && expectation.ship_in_space_ballpark == Some(false) {
        violations.push(LegalFixtureViolation::ShipMissingFromSpaceBallpark {
            ship_id,
            solar_system_id: sys_id,
        });
    }
    Some((ship_id, ship_type))
}

fn validate_ship_skills_and_fitting(
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    char_id: i64,
    ship_id: i64,
    ship_type: &InvType,
    violations: &mut Vec<LegalFixtureViolation>,
) {
    let empty_skills = HashMap::new();
    let char_skills = snap.skills.get(&char_id).unwrap_or(&empty_skills);

    let check_skills = |for_item_id: i64, for_type_id: TypeId, out: &mut Vec<LegalFixtureViolation>| {
        let mut reqs = BTreeMap::new();
        let mut visited = HashSet::new();
        collect_required_skills_recursive(sde, for_type_id, &mut reqs, &mut visited, 0);
        for (skill_type_id, req_lvl) in reqs {
            let actual_lvl = char_skills.get(&skill_type_id).map(|r| r.level).unwrap_or(0);
            if actual_lvl < req_lvl {
                out.push(LegalFixtureViolation::MissingRequiredSkill {
                    character_id: char_id,
                    for_item_id,
                    for_type_id,
                    skill_type_id,
                    required_level: req_lvl,
                    actual_level: actual_lvl,
                });
            }
        }
    };

    check_skills(ship_id, ship_type.type_id, violations);

    let mut ship_slot_items: Vec<&InventoryItemRecord> = snap
        .items
        .values()
        .filter(|it| it.location_id == ship_id && is_fitted_slot_flag(it.flag as u16))
        .collect();
    ship_slot_items.sort_by_key(|it| (it.flag as u16, it.item_id));

    let mut modules_by_flag: BTreeMap<u16, (&InventoryItemRecord, &InvType)> = BTreeMap::new();
    let mut charges: Vec<(&InventoryItemRecord, &InvType)> = Vec::new();

    for it in ship_slot_items {
        if it.owner_id != char_id {
            violations.push(LegalFixtureViolation::UnownedFittedItem {
                ship_id,
                item_id: it.item_id,
                type_id: it.type_id,
                expected_owner_id: char_id,
                actual_owner_id: it.owner_id,
            });
        }
        let Some(inv_type) = sde.types.get(&it.type_id) else {
            violations.push(LegalFixtureViolation::InvalidFittedModule {
                ship_id,
                item_id: it.item_id,
                type_id: it.type_id,
                reason: "unknown SDE type_id".to_string(),
            });
            continue;
        };
        let flag_u16 = it.flag as u16;
        if inv_type.category_id == 8 && is_module_rack_flag(flag_u16) {
            charges.push((it, inv_type));
            continue;
        }
        if !matches!(inv_type.category_id, 7 | 32) {
            violations.push(LegalFixtureViolation::InvalidFittedModule {
                ship_id,
                item_id: it.item_id,
                type_id: it.type_id,
                reason: format!(
                    "fitted item category_id {} is not Module (7) or Subsystem (32)",
                    inv_type.category_id
                ),
            });
            continue;
        }
        if !it.singleton || it.quantity != 1 {
            violations.push(LegalFixtureViolation::InvalidFittedModule {
                ship_id,
                item_id: it.item_id,
                type_id: it.type_id,
                reason: format!(
                    "fitted module must be singleton with quantity 1 (singleton={}, quantity={})",
                    it.singleton, it.quantity
                ),
            });
        }
        check_skills(it.item_id, it.type_id, violations);
        if let Some(&(first_item, _)) = modules_by_flag.get(&flag_u16) {
            violations.push(LegalFixtureViolation::DuplicateSlotModule {
                ship_id,
                slot_flag: flag_u16,
                first_item_id: first_item.item_id,
                duplicate_item_id: it.item_id,
                type_id: it.type_id,
            });
        } else {
            modules_by_flag.insert(flag_u16, (it, inv_type));
        }
    }

    for (charge_item, charge_type) in charges {
        let flag_u16 = charge_item.flag as u16;
        if charge_item.quantity <= 0 {
            violations.push(LegalFixtureViolation::InvalidLoadedChargeQuantity {
                ship_id,
                slot_flag: flag_u16,
                charge_item_id: charge_item.item_id,
                charge_type_id: charge_type.type_id,
                quantity: charge_item.quantity,
            });
        }
        check_skills(charge_item.item_id, charge_type.type_id, violations);
        let Some(&(mod_item, mod_type)) = modules_by_flag.get(&flag_u16) else {
            violations.push(LegalFixtureViolation::OrphanLoadedCharge {
                ship_id,
                charge_item_id: charge_item.item_id,
                charge_type_id: charge_type.type_id,
                slot_flag: flag_u16,
            });
            continue;
        };
        if !is_charge_compatible_with_module(mod_type, charge_type) {
            violations.push(LegalFixtureViolation::IncompatibleLoadedCharge {
                ship_id,
                slot_flag: flag_u16,
                module_item_id: mod_item.item_id,
                module_type_id: mod_type.type_id,
                charge_item_id: charge_item.item_id,
                charge_type_id: charge_type.type_id,
            });
        }
        if charge_item.quantity > 0 {
            let mod_cap = if mod_type.capacity > 0.0 {
                mod_type.capacity
            } else {
                mod_type.default_attributes.get(&38).copied().unwrap_or(0.0)
            };
            let charge_vol = if charge_type.volume > 0.0 {
                charge_type.volume
            } else {
                charge_type.default_attributes.get(&161).copied().unwrap_or(0.0)
            };
            let total_vol = f64::from(charge_item.quantity) * charge_vol;
            let exceeds = if mod_cap > 0.0 && charge_vol > 0.0 {
                total_vol > mod_cap + 1e-9
            } else {
                mod_cap <= 0.0 && charge_item.quantity > 1
            };
            if exceeds {
                violations.push(LegalFixtureViolation::ChargeExceedsModuleCapacity {
                    ship_id,
                    slot_flag: flag_u16,
                    module_item_id: mod_item.item_id,
                    module_type_id: mod_type.type_id,
                    charge_item_id: charge_item.item_id,
                    charge_type_id: charge_type.type_id,
                    quantity: charge_item.quantity,
                    total_volume_milli: (total_vol * 1000.0).round() as i64,
                    module_capacity_milli: (mod_cap * 1000.0).round() as i64,
                });
            }
        }
    }
}

/// Validates that `(snap, sde, session, expectation)` describes a reachable, legal EVE fixture.
pub fn check_legal_fixture(
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    session: &FixtureSessionView,
    expectation: &LegalFixtureExpectation,
) -> Result<(), Vec<LegalFixtureViolation>> {
    let mut violations = Vec::new();

    let Some(user_id) = session.user_id else {
        violations.push(LegalFixtureViolation::UnauthenticatedSession {
            session_id: session.session_id,
        });
        return Err(violations);
    };
    let account = snap.accounts.values().find(|a| a.user_id == user_id);
    if account.is_none_or(|a| match (a.banned_until_unix, session.now_unix) {
        (Some(until), Some(now)) => now < until,
        (Some(_), None) => true,
        (None, _) => false,
    }) {
        violations.push(LegalFixtureViolation::MissingOrBannedAccount { user_id });
    }

    let Some(char_id) = session.character_id else {
        violations.push(LegalFixtureViolation::UnauthenticatedSession {
            session_id: session.session_id,
        });
        return Err(violations);
    };
    let Some(ch) = snap.characters.get(&char_id) else {
        violations.push(LegalFixtureViolation::MissingCharacter {
            character_id: char_id,
        });
        return Err(violations);
    };
    if ch.user_id != user_id {
        violations.push(LegalFixtureViolation::CharacterAccountMismatch {
            character_id: char_id,
            session_user_id: user_id,
            owner_user_id: ch.user_id,
        });
    }
    if expectation.expect_omega && ch.clone_grade < 2 {
        violations.push(LegalFixtureViolation::NonOmegaCharacter {
            character_id: char_id,
            clone_grade: ch.clone_grade,
        });
    }

    validate_corporation_and_roles(snap, sde, session, ch, char_id, expectation, &mut violations);

    if let Some((ship_id, ship_type)) =
        validate_location_and_ship(snap, sde, session, ch, char_id, expectation, &mut violations)
    {
        validate_ship_skills_and_fitting(snap, sde, char_id, ship_id, &ship_type, &mut violations);
    }

    if expectation.require_fleet_membership && session.fleet_id.is_none() {
        violations.push(LegalFixtureViolation::FleetMembershipMismatch {
            character_id: char_id,
            fleet_id: None,
            reason: "fleet membership required but session.fleet_id is None".to_string(),
        });
    } else if let Some(fleet_id) = session.fleet_id {
        match snap.fleets.get(&fleet_id) {
            None => violations.push(LegalFixtureViolation::FleetMembershipMismatch {
                character_id: char_id,
                fleet_id: Some(fleet_id),
                reason: "session references missing fleet_id".to_string(),
            }),
            Some(fleet) if !fleet.members.contains(&char_id) => {
                violations.push(LegalFixtureViolation::FleetMembershipMismatch {
                    character_id: char_id,
                    fleet_id: Some(fleet_id),
                    reason: "character is absent from fleet.members".to_string(),
                });
            }
            Some(_) => {}
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

/// Asserts that [`check_legal_fixture`] succeeds, panicking loudly with every violation if not.
pub fn assert_legal_fixture(
    context_label: &str,
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    session: &FixtureSessionView,
    expectation: &LegalFixtureExpectation,
) {
    if let Err(violations) = check_legal_fixture(snap, sde, session, expectation) {
        let joined: Vec<String> = violations.iter().map(ToString::to_string).collect();
        panic!(
            "Illegal test fixture in {context_label} ({} violation(s)):\n  {}",
            joined.len(),
            joined.join("\n  ")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eve_db::{AccountRecord, TrainedSkillRecord};
    use eve_sde::{CategoryId, GroupId};

    fn mk_type(type_id: TypeId, group_id: GroupId, category_id: CategoryId, name: &str, attrs: &[(u16, f64)]) -> InvType {
        InvType {
            type_id, group_id, category_id, name: name.to_string(),
            mass: 1.0, volume: if category_id == 8 { 0.01 } else { 1.0 }, capacity: 5.0, radius: 1.0, base_price: 100.0,
            published: true, portion_size: 1, market_group_id: None, race_id: None,
            faction_id: None, variation_parent_type_id: None, description_id: None,
            icon_id: None, graphic_id: None, sound_id: None, meta_group_id: None,
            meta_level: None, tech_level: None, default_effect_id: None,
            description: String::new(), effect_ids: Vec::new(),
            default_attributes: attrs.iter().copied().collect(),
        }
    }

    fn base_valid_world() -> (WorldStateSnapshot, SdeRegistry, FixtureSessionView) {
        let mut sde = SdeRegistry::default();
        sde.types.insert(3300, mk_type(3300, 257, 16, "Gunnery", &[]));
        sde.types.insert(3302, mk_type(3302, 257, 16, "Small Projectile Turret", &[]));
        sde.types.insert(3329, mk_type(3329, 257, 16, "Minmatar Frigate", &[]));
        sde.types.insert(587, mk_type(587, 25, 6, "Rifter", &[(182, 3329.0), (277, 1.0)]));
        sde.types.insert(484, mk_type(484, 55, 7, "125mm Gatling AutoCannon I", &[(182, 3302.0), (277, 1.0), (604, 83.0), (128, 1.0)]));
        sde.types.insert(178, mk_type(178, 83, 8, "Carbonized Lead S", &[(128, 1.0)]));
        sde.types.insert(186, mk_type(186, 83, 8, "Carbonized Lead M", &[(128, 2.0)]));
        sde.types.insert(234, mk_type(234, 85, 8, "Antimatter Charge S", &[(128, 1.0)]));

        let mut snap = WorldStateSnapshot::default();
        snap.accounts.insert("pilot".into(), AccountRecord {
            user_id: 10, username: "pilot".into(), password_token: "hash".into(),
            role: eve_db::service_roles::ROLE_PLAYER_DEFAULT, banned_until_unix: None, ban_reason: None,
        });
        snap.characters.insert(90_000_001, CharacterRecord {
            character_id: 90_000_001, user_id: 10, name: "Pilot One".into(),
            corporation_id: 98_000_001, solar_system_id: 30_000_141, active_ship_id: 500_001,
            ..Default::default()
        });
        let mut corp = CorporationRecord::new(98_000_001, "Test Corp", "TCRP", 90_000_001);
        corp.members.insert(90_000_001);
        corp.member_roles.insert(90_000_001, corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(98_000_001, corp);
        snap.skills.insert(90_000_001, HashMap::from([
            (3300, TrainedSkillRecord { type_id: 3300, level: 2, skill_points: 1000.0 }),
            (3302, TrainedSkillRecord { type_id: 3302, level: 1, skill_points: 250.0 }),
            (3329, TrainedSkillRecord { type_id: 3329, level: 1, skill_points: 250.0 }),
        ]));
        snap.items.insert(500_001, InventoryItemRecord { item_id: 500_001, type_id: 587, owner_id: 90_000_001, location_id: 30_000_141, flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() });
        snap.items.insert(500_002, InventoryItemRecord { item_id: 500_002, type_id: 484, owner_id: 90_000_001, location_id: 500_001, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default() });
        snap.items.insert(500_003, InventoryItemRecord { item_id: 500_003, type_id: 178, owner_id: 90_000_001, location_id: 500_001, flag: InventoryFlag::HiSlot0, quantity: 40, singleton: false, ..Default::default() });
        let sess = FixtureSessionView {
            session_id: 1, user_id: Some(10), character_id: Some(90_000_001),
            corporation_id: Some(98_000_001), solar_system_id: Some(30_000_141),
            ship_id: Some(500_001), corp_role: corp_roles::ROLE_DIRECTOR, ..Default::default()
        };
        (snap, sde, sess)
    }

    #[test]
    fn legal_fixture_accepts_valid_world_and_rejects_broken_invariants() {
        let (snap, sde, sess) = base_valid_world();
        let exp = LegalFixtureExpectation {
            require_player_corp: true, expected_corp_roles: corp_roles::ROLE_DIRECTOR,
            expect_in_space: Some(true), ship_in_space_ballpark: Some(true), ..Default::default()
        };
        assert_legal_fixture("valid-combat", &snap, &sde, &sess, &exp);

        let mut no_skills = snap.clone();
        no_skills.skills.get_mut(&90_000_001).unwrap().remove(&3302);
        let errs = check_legal_fixture(&no_skills, &sde, &sess, &exp).unwrap_err();
        assert!(errs.iter().any(|v| matches!(v, LegalFixtureViolation::MissingRequiredSkill { skill_type_id: 3302, .. })), "{errs:?}");

        for bad_ammo in [234_u32, 186_u32] {
            let mut bad_charge = snap.clone();
            bad_charge.items.get_mut(&500_003).unwrap().type_id = bad_ammo;
            let errs = check_legal_fixture(&bad_charge, &sde, &sess, &exp).unwrap_err();
            assert!(errs.iter().any(|v| matches!(v, LegalFixtureViolation::IncompatibleLoadedCharge { charge_type_id, .. } if *charge_type_id == bad_ammo)), "{errs:?}");
        }

        let mut npc_corp = snap.clone();
        npc_corp.characters.get_mut(&90_000_001).unwrap().corporation_id = 1_000_167;
        let mut npc_sess = sess.clone();
        npc_sess.corporation_id = Some(1_000_167);
        let errs = check_legal_fixture(&npc_corp, &sde, &npc_sess, &exp).unwrap_err();
        assert!(errs.iter().any(|v| matches!(v, LegalFixtureViolation::NpcCorpUsedAsPlayerCorp { corporation_id: 1_000_167, .. })), "{errs:?}");

        let mut dock_mismatch = snap.clone();
        dock_mismatch.items.get_mut(&500_001).unwrap().location_id = 60_015_157;
        dock_mismatch.items.get_mut(&500_001).unwrap().flag = InventoryFlag::Hangar;
        let errs = check_legal_fixture(&dock_mismatch, &sde, &sess, &exp).unwrap_err();
        assert!(errs.iter().any(|v| matches!(v, LegalFixtureViolation::ShipLocationMismatch { .. })), "{errs:?}");

        let mut unauth = sess.clone();
        unauth.user_id = None;
        let errs = check_legal_fixture(&snap, &sde, &unauth, &exp).unwrap_err();
        assert!(errs.iter().any(|v| matches!(v, LegalFixtureViolation::UnauthenticatedSession { .. })), "{errs:?}");
    }

    #[test]
    fn legal_fixture_rejects_charge_capacity_skills_ownership_duplicate_slots_bans_and_alpha_omega() {
        let (mut snap, mut sde, mut sess) = base_valid_world();
        sess.now_unix = Some(1_000_000);
        snap.characters.get_mut(&90_000_001).unwrap().clone_grade = 2;
        let exp = LegalFixtureExpectation {
            require_player_corp: true,
            expected_corp_roles: corp_roles::ROLE_DIRECTOR,
            expect_in_space: Some(true),
            ship_in_space_ballpark: Some(true),
            expect_omega: true,
            ..Default::default()
        };
        assert_legal_fixture("valid-omega-with-now", &snap, &sde, &sess, &exp);

        // Charge quantity <= 0
        for bad_qty in [0_i32, -3_i32] {
            let mut bad = snap.clone();
            bad.items.get_mut(&500_003).unwrap().quantity = bad_qty;
            let errs = check_legal_fixture(&bad, &sde, &sess, &exp).unwrap_err();
            assert!(errs.iter().any(|v| matches!(v, LegalFixtureViolation::InvalidLoadedChargeQuantity { quantity, .. } if *quantity == bad_qty)), "{errs:?}");
        }

        // Charge total volume exceeds InvType.capacity (501 * 0.01 = 5.01 > 5.0)
        let mut overcap = snap.clone();
        overcap.items.get_mut(&500_003).unwrap().quantity = 501;
        let errs = check_legal_fixture(&overcap, &sde, &sess, &exp).unwrap_err();
        assert!(errs.iter().any(|v| matches!(v, LegalFixtureViolation::ChargeExceedsModuleCapacity { quantity: 501, .. })), "{errs:?}");

        // Attribute fallback check when InvType.capacity == 0.0 and InvType.volume == 0.0 (attrs 38 & 161)
        let mut attr_sde = sde.clone();
        let mod_t = attr_sde.types.get_mut(&484).unwrap();
        mod_t.capacity = 0.0;
        mod_t.default_attributes.insert(38, 0.25);
        let chg_t = attr_sde.types.get_mut(&178).unwrap();
        chg_t.volume = 0.0;
        chg_t.default_attributes.insert(161, 0.01);
        let errs = check_legal_fixture(&snap, &attr_sde, &sess, &exp).unwrap_err(); // 40 * 0.01 = 0.40 > 0.25
        assert!(errs.iter().any(|v| matches!(v, LegalFixtureViolation::ChargeExceedsModuleCapacity { module_capacity_milli: 250, .. })), "{errs:?}");

        // Loaded charge prerequisite skills (attr 182 = 3302 at level 3 vs trained level 1)
        let mut skill_sde = sde.clone();
        skill_sde.types.get_mut(&178).unwrap().default_attributes.insert(182, 3302.0);
        skill_sde.types.get_mut(&178).unwrap().default_attributes.insert(277, 3.0);
        let errs = check_legal_fixture(&snap, &skill_sde, &sess, &exp).unwrap_err();
        assert!(errs.iter().any(|v| matches!(v, LegalFixtureViolation::MissingRequiredSkill { for_item_id: 500_003, for_type_id: 178, skill_type_id: 3302, required_level: 3, actual_level: 1, .. })), "{errs:?}");

        // Non-owner fitted module & non-owner loaded charge
        for item_id in [500_002_i64, 500_003_i64] {
            let mut wrong_owner = snap.clone();
            wrong_owner.items.get_mut(&item_id).unwrap().owner_id = 90_000_999;
            let errs = check_legal_fixture(&wrong_owner, &sde, &sess, &exp).unwrap_err();
            assert!(errs.iter().any(|v| matches!(v, LegalFixtureViolation::UnownedFittedItem { item_id: id, actual_owner_id: 90_000_999, .. } if *id == item_id)), "{errs:?}");
        }

        // Duplicate fitted module sharing the same slot flag
        let mut dup_slot = snap.clone();
        dup_slot.items.insert(500_004, InventoryItemRecord {
            item_id: 500_004, type_id: 484, owner_id: 90_000_001, location_id: 500_001,
            flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        let errs = check_legal_fixture(&dup_slot, &sde, &sess, &exp).unwrap_err();
        assert!(errs.iter().any(|v| matches!(v, LegalFixtureViolation::DuplicateSlotModule { slot_flag: 27, first_item_id: 500_002, duplicate_item_id: 500_004, .. })), "{errs:?}");

        // Active account ban rejected vs expired ban timestamp accepted
        let mut active_ban = snap.clone();
        active_ban.accounts.get_mut("pilot").unwrap().banned_until_unix = Some(1_000_060);
        let errs = check_legal_fixture(&active_ban, &sde, &sess, &exp).unwrap_err();
        assert!(errs.iter().any(|v| matches!(v, LegalFixtureViolation::MissingOrBannedAccount { user_id: 10 })), "{errs:?}");

        let mut expired_ban = snap.clone();
        expired_ban.accounts.get_mut("pilot").unwrap().banned_until_unix = Some(999_999);
        assert_legal_fixture("expired-ban-accepted", &expired_ban, &sde, &sess, &exp);

        // Omega clone_grade requirement
        let mut alpha = snap.clone();
        alpha.characters.get_mut(&90_000_001).unwrap().clone_grade = 1;
        let errs = check_legal_fixture(&alpha, &sde, &sess, &exp).unwrap_err();
        assert!(errs.iter().any(|v| matches!(v, LegalFixtureViolation::NonOmegaCharacter { character_id: 90_000_001, clone_grade: 1 })), "{errs:?}");

        // Capacity-less module (capacity == 0.0, no attr 38) permits qty 1 and rejects qty > 1
        sde.types.get_mut(&484).unwrap().capacity = 0.0;
        let mut capless_ok = snap.clone();
        capless_ok.items.get_mut(&500_003).unwrap().quantity = 1;
        assert_legal_fixture("capless-single-charge", &capless_ok, &sde, &sess, &exp);
        let errs = check_legal_fixture(&snap, &sde, &sess, &exp).unwrap_err(); // qty 40 > 1
        assert!(errs.iter().any(|v| matches!(v, LegalFixtureViolation::ChargeExceedsModuleCapacity { quantity: 40, .. })), "{errs:?}");
    }
}
