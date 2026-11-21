//! Canonical database-authoritative corporation actor and role resolution.
//!
//! All corporation membership, exclusion, NPC detection, alliance attribution, and effective role
//! resolution compute exclusively from [`WorldStateSnapshot`] and [`SdeRegistry`] without using
//! mutable `SessionState` fallbacks.

#![forbid(unsafe_code)]

use crate::snapshot_ext::title_id_to_bit;
use crate::{corp_roles, CharacterRecord, CorporationRecord, WorldStateSnapshot};
use eve_sde::SdeRegistry;

/// Reasons DB-only corporation actor authorization can fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpActorDenied {
    NoCorporation,
    NpcCorporation,
    CorporationNotFound,
    NotMember,
    RoleDenied,
}

/// Database-resolved corporation membership, alliance, and effective role state for one character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpActor {
    pub char_id: i64,
    pub char_in_db: bool,
    pub corp_id: i64,
    pub alliance_id: Option<i64>,
    pub corp_exists: bool,
    pub is_npc_corp: bool,
    pub is_member: bool,
    pub is_excluded: bool,
    pub is_ceo: bool,
    pub effective_roles: u64,
}

/// Returns `true` when `corp_id` cannot represent a player corporation (`<= 0`, NPC range, or SDE NPC corp).
pub fn is_npc_corporation(sde: &SdeRegistry, corp_id: i64) -> bool {
    corp_id <= 0
        || (1_000_000..2_000_000).contains(&corp_id)
        || sde.npc_corporations.contains_key(&corp_id)
}

/// Returns `true` when `corp` tracks an explicit non-empty member roster and `char_id` is neither CEO nor listed.
pub fn is_excluded_corp_member(corp: &CorporationRecord, char_id: i64) -> bool {
    corp.ceo_id != char_id && !corp.members.is_empty() && !corp.members.contains(&char_id)
}

/// Resolves authoritative DB `alliance_id` for `corp_id`, treating `CorporationRecord` and `AllianceRecord`
/// as authoritative over character-row `alliance_id` whenever the corporation record exists in `snap`.
pub fn resolve_corp_alliance_id(
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    corp_id: i64,
    ch: Option<&CharacterRecord>,
) -> Option<i64> {
    if is_npc_corporation(sde, corp_id) {
        return None;
    }
    if let Some(corp) = snap.corporations.get(&corp_id) {
        return corp
            .alliance_id
            .or_else(|| {
                snap.alliances
                    .values()
                    .find(|a| a.member_corp_ids.contains(&corp_id))
                    .map(|a| a.alliance_id)
            })
            .filter(|&id| id > 0);
    }
    ch.and_then(|c| (c.corporation_id == corp_id).then_some(c.alliance_id).flatten())
        .or_else(|| {
            snap.alliances
                .values()
                .find(|a| a.member_corp_ids.contains(&corp_id))
                .map(|a| a.alliance_id)
        })
        .filter(|&id| id > 0)
}

fn resolve_loc_station_id(
    snap: &WorldStateSnapshot,
    char_id: i64,
    location_id: Option<i64>,
    fallback_to_char_location: bool,
) -> i64 {
    let mut loc_station_id = location_id
        .or_else(|| {
            fallback_to_char_location
                .then(|| {
                    snap.characters
                        .get(&char_id)
                        .and_then(|ch| ch.station_id.or(ch.structure_id))
                })
                .flatten()
        })
        .unwrap_or(0);
    for _ in 0..8 {
        if loc_station_id <= 0
            || (60_000_000..=69_999_999).contains(&loc_station_id)
            || loc_station_id >= 1_000_000_000_000
            || snap.structures.contains_key(&loc_station_id)
        {
            break;
        }
        let Some(it) = snap.items.get(&loc_station_id) else {
            break;
        };
        if it.location_id <= 0 || it.location_id == loc_station_id {
            break;
        }
        loc_station_id = it.location_id;
    }
    loc_station_id
}

pub(crate) fn evaluate_corp_roles_for_record(
    snap: &WorldStateSnapshot,
    corp_id: i64,
    corp: &CorporationRecord,
    char_id: i64,
    location_id: Option<i64>,
    fallback_to_char_location: bool,
    session_roles_fallback: Option<u64>,
    now_unix: u64,
) -> (bool, u64) {
    if corp.ceo_id == char_id {
        return (true, u64::MAX);
    }
    let has_ceo_roles_entry = corp.member_roles.contains_key(&corp.ceo_id);
    let is_corp_member = corp.members.contains(&char_id)
        || (!has_ceo_roles_entry
            && snap
                .characters
                .get(&char_id)
                .is_some_and(|ch| ch.corporation_id == corp_id))
        || (corp.members.is_empty() && !snap.characters.contains_key(&char_id));
    let mut roles = if let Some(&db_roles) = corp.member_roles.get(&char_id) {
        db_roles
    } else if snap.corp_role_assignments.contains_key(&(corp_id, char_id))
        || !is_corp_member
        || session_roles_fallback.is_none_or(|sr| sr == u64::MAX)
    {
        0
    } else {
        session_roles_fallback.unwrap_or(0)
    };

    let char_home = snap
        .characters
        .get(&char_id)
        .and_then(|ch| {
            ch.station_id
                .or(ch.structure_id)
                .or(Some(ch.home_station_id))
                .filter(|&id| id > 0)
        })
        .unwrap_or(0);
    let hq_station_id = corp.hq_station_id.unwrap_or(char_home);
    let loc_station_id =
        resolve_loc_station_id(snap, char_id, location_id, fallback_to_char_location);
    let gov_opt = snap.corp_governance.get(&corp_id);
    let base_id = gov_opt
        .and_then(|g| g.member_bases.get(&char_id).copied())
        .unwrap_or(0);

    if let Some(ra) = snap.corp_role_assignments.get(&(corp_id, char_id)) {
        roles |= ra.roles;
        if loc_station_id > 0 {
            if loc_station_id == hq_station_id {
                roles |= ra.roles_at_hq;
            }
            if base_id > 0 && loc_station_id == base_id {
                roles |= ra.roles_at_base;
            }
            if loc_station_id != hq_station_id && (base_id <= 0 || loc_station_id != base_id) {
                roles |= ra.roles_at_other;
            }
        }
    }

    let mut unmatured_role_bits = 0u64;
    let mut unmatured_title_bits = 0u64;
    if let Some(gov) = gov_opt {
        if let Some(&(p_mask, matured_at)) = gov.pending_role_grants.get(&char_id) {
            if now_unix >= matured_at {
                roles |= p_mask;
            } else {
                // Only roles subject to 24h stasis (`CORP_DELAYED_DIVISION_ROLE_MASK`, Director,
                // and Container Take bits) are suppressed while pending; `UpdateMember` also packs
                // immediate roles like Accountant/Trader into `pending_role_grants`.
                const DELAYED_STASIS_MASK: u64 = corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK
                    | corp_roles::ROLE_DIRECTOR
                    | (0x7Fu64 << 42);
                unmatured_role_bits |= p_mask & DELAYED_STASIS_MASK;
            }
        }
        if let Some(&(hq_p, base_p, other_p, matured_at)) =
            gov.pending_scoped_role_grants.get(&char_id)
        {
            if loc_station_id > 0 {
                let mut scoped_p = 0u64;
                if loc_station_id == hq_station_id {
                    scoped_p |= hq_p;
                }
                if base_id > 0 && loc_station_id == base_id {
                    scoped_p |= base_p;
                }
                if loc_station_id != hq_station_id && (base_id <= 0 || loc_station_id != base_id) {
                    scoped_p |= other_p;
                }
                if now_unix >= matured_at {
                    roles |= scoped_p;
                } else {
                    unmatured_role_bits |= scoped_p;
                }
            }
        }
        if let Some(&(t_mask, matured_at)) = gov.pending_title_role_grants.get(&char_id) {
            if now_unix >= matured_at {
                roles |= t_mask;
            } else {
                unmatured_title_bits |= t_mask;
            }
        }
        if let Some(&tmask) = gov.member_titles.get(&char_id) {
            for (&tid, &(_, troles)) in &gov.titles {
                let bit = title_id_to_bit(tid);
                if (tmask & bit) != 0 {
                    roles |= troles & !unmatured_title_bits;
                }
            }
        }
    }

    let member_tmask = gov_opt
        .and_then(|g| g.member_titles.get(&char_id).copied())
        .unwrap_or(0);
    let tids_opt = snap.corp_member_titles.get(&(corp_id, char_id));
    if let Some(titles) = snap.corp_titles.get(&corp_id) {
        for t in titles.iter().filter(|t| {
            tids_opt.is_some_and(|tids| tids.contains(&t.title_id))
                || (member_tmask & title_id_to_bit(t.title_id)) != 0
        }) {
            roles |= corp_roles::encode_corp_role_names(&t.roles) & !unmatured_title_bits;
            if loc_station_id > 0 {
                if loc_station_id == hq_station_id {
                    roles |= corp_roles::encode_corp_role_names(&t.roles_at_hq)
                        & !unmatured_title_bits;
                }
                if base_id > 0 && loc_station_id == base_id {
                    roles |= corp_roles::encode_corp_role_names(&t.roles_at_base)
                        & !unmatured_title_bits;
                }
                if loc_station_id != hq_station_id && (base_id <= 0 || loc_station_id != base_id) {
                    roles |= corp_roles::encode_corp_role_names(&t.roles_at_other)
                        & !unmatured_title_bits;
                }
            }
        }
    }

    roles &= !(unmatured_role_bits | unmatured_title_bits);
    (false, roles)
}

/// Computes `(is_ceo, effective_roles)` purely from `snap` without any `session_roles` fallback.
pub fn effective_corp_roles_at_db(
    snap: &WorldStateSnapshot,
    corp_id: i64,
    char_id: i64,
    location_id: Option<i64>,
    now_unix: u64,
) -> (bool, u64) {
    let Some(corp) = snap.corporations.get(&corp_id) else {
        return (false, 0);
    };
    evaluate_corp_roles_for_record(snap, corp_id, corp, char_id, location_id, true, None, now_unix)
}

pub(crate) fn effective_corp_roles_at_with_fallback(
    snap: &WorldStateSnapshot,
    corp_id: i64,
    char_id: i64,
    location_id: Option<i64>,
    session_roles: u64,
    now_unix: u64,
) -> (bool, u64) {
    let Some(corp) = snap.corporations.get(&corp_id) else {
        let mut roles = session_roles;
        if let Some(ra) = snap.corp_role_assignments.get(&(corp_id, char_id)) {
            roles |= ra.roles | ra.roles_at_hq | ra.roles_at_base | ra.roles_at_other;
        }
        return (false, roles);
    };
    evaluate_corp_roles_for_record(
        snap,
        corp_id,
        corp,
        char_id,
        location_id,
        true,
        Some(session_roles),
        now_unix,
    )
}

impl CorpActor {
    /// Resolves `char_id`'s DB corporation membership, alliance, and global effective roles.
    pub fn resolve(
        snap: &WorldStateSnapshot,
        sde: &SdeRegistry,
        char_id: i64,
        now_unix: u64,
    ) -> Self {
        Self::resolve_at(snap, sde, char_id, None, now_unix)
    }

    /// Resolves `char_id`'s DB corporation membership, alliance, and effective roles at `location_id`.
    pub fn resolve_at(
        snap: &WorldStateSnapshot,
        sde: &SdeRegistry,
        char_id: i64,
        location_id: Option<i64>,
        now_unix: u64,
    ) -> Self {
        let corp_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .unwrap_or(0);
        Self::resolve_for_corp(snap, sde, corp_id, char_id, location_id, now_unix)
    }

    /// Resolves `char_id`'s DB authority specifically against `corp_id` at `location_id`.
    pub fn resolve_for_corp(
        snap: &WorldStateSnapshot,
        sde: &SdeRegistry,
        corp_id: i64,
        char_id: i64,
        location_id: Option<i64>,
        now_unix: u64,
    ) -> Self {
        let ch = snap.characters.get(&char_id);
        let char_in_db = ch.is_some();
        let is_npc_corp = is_npc_corporation(sde, corp_id);
        let corp_opt = snap.corporations.get(&corp_id);
        let corp_exists = corp_opt.is_some();
        let alliance_id = resolve_corp_alliance_id(snap, sde, corp_id, ch);

        if is_npc_corp {
            return Self {
                char_id,
                char_in_db,
                corp_id,
                alliance_id: None,
                corp_exists,
                is_npc_corp: true,
                is_member: ch.is_some_and(|c| c.corporation_id == corp_id),
                is_excluded: false,
                is_ceo: false,
                effective_roles: 0,
            };
        }

        let Some(corp) = corp_opt else {
            return Self {
                char_id,
                char_in_db,
                corp_id,
                alliance_id,
                corp_exists: false,
                is_npc_corp: false,
                is_member: false,
                is_excluded: false,
                is_ceo: false,
                effective_roles: 0,
            };
        };

        let wrong_char_corp = ch.is_some_and(|c| c.corporation_id != corp_id);
        let is_excluded = is_excluded_corp_member(corp, char_id);
        let is_member = !wrong_char_corp
            && (corp.ceo_id == char_id
                || !is_excluded
                    && (!corp.members.is_empty()
                        || ch.is_some_and(|c| c.corporation_id == corp_id)));
        let (is_ceo, effective_roles) = if is_member {
            evaluate_corp_roles_for_record(
                snap,
                corp_id,
                corp,
                char_id,
                location_id,
                false,
                None,
                now_unix,
            )
        } else {
            (false, 0)
        };

        Self {
            char_id,
            char_in_db,
            corp_id,
            alliance_id,
            corp_exists: true,
            is_npc_corp: false,
            is_member,
            is_excluded,
            is_ceo,
            effective_roles,
        }
    }

    pub fn is_player_corp_member(&self) -> bool {
        !self.is_npc_corp && self.corp_exists && self.is_member && !self.is_excluded
    }

    pub fn is_director(&self) -> bool {
        self.is_player_corp_member()
            && (self.is_ceo || (self.effective_roles & corp_roles::ROLE_DIRECTOR) != 0)
    }

    /// Checks whether all role bits in `required_all` (or `ROLE_DIRECTOR`/CEO) are held.
    pub fn has_role(&self, required_all: u64) -> bool {
        self.is_player_corp_member()
            && (self.is_ceo || corp_roles::has_role(self.effective_roles, required_all))
    }

    /// Checks whether any role bit in `any_mask` (or `ROLE_DIRECTOR`/CEO) is held.
    pub fn has_any_role(&self, any_mask: u64) -> bool {
        self.is_player_corp_member()
            && (self.is_ceo
                || corp_roles::has_role(self.effective_roles, corp_roles::ROLE_DIRECTOR)
                || (self.effective_roles & any_mask) != 0)
    }

    /// Convenience alias for `has_any_role(corp_roles::ROLE_DIRECTOR | any_mask)`.
    pub fn is_director_or(&self, any_mask: u64) -> bool {
        self.has_any_role(corp_roles::ROLE_DIRECTOR | any_mask)
    }

    pub fn role_tuple(&self) -> (bool, u64) {
        if self.is_player_corp_member() {
            (self.is_ceo, self.effective_roles)
        } else {
            (false, 0)
        }
    }

    pub fn require_member(&self) -> Result<(), CorpActorDenied> {
        if !self.char_in_db || self.corp_id <= 0 {
            return Err(CorpActorDenied::NoCorporation);
        }
        if self.is_npc_corp {
            return Err(CorpActorDenied::NpcCorporation);
        }
        if !self.corp_exists {
            return Err(CorpActorDenied::CorporationNotFound);
        }
        if !self.is_member || self.is_excluded {
            return Err(CorpActorDenied::NotMember);
        }
        Ok(())
    }

    pub fn require_role(&self, required_all: u64) -> Result<(), CorpActorDenied> {
        self.require_member()?;
        if self.is_ceo || corp_roles::has_role(self.effective_roles, required_all) {
            Ok(())
        } else {
            Err(CorpActorDenied::RoleDenied)
        }
    }

    pub fn require_any_role(&self, any_mask: u64) -> Result<(), CorpActorDenied> {
        self.require_member()?;
        if self.has_any_role(any_mask) {
            Ok(())
        } else {
            Err(CorpActorDenied::RoleDenied)
        }
    }
}

#[cfg(test)]
#[path = "corp_actor_tests.rs"]
mod corp_actor_tests;
