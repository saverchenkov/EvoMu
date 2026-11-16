//! SQLite persistence for Family 4: Corporation Governance, Crimewatch, and Warfare (`#916`).
//!
//! Decomposes all 36 persisted `WorldStateSnapshot` fields across `corp_governance`,
//! `crimewatch`, and `warfare` (including inner maps/sets/scalars of the 6 `Persisted*State`
//! runtime structs) into canonical `domain_rows` entries under `corp_crimewatch.*`, and
//! handles one-shot startup migration + drop of retired legacy `d6_corp_crimewatch_kv`.

use super::domain_rows::{
    decode_map_entry, decode_set_entry, diff_map_with_mapped, diff_scalar, diff_set_with_prefix,
    fetch_domain_row_entries, format_domain_field_key, from_checked_domain_json, table_exists,
    DomainRowChange, DomainSyncState, LegacyMerge, Persist, DOMAIN_CORP_GOVERNANCE,
    DOMAIN_CRIMEWATCH, DOMAIN_WARFARE,
};
use crate::durable_domain_types::{
    tuple_map_serde, CorpGovState, PersistedAggressionRecord, PersistedAllianceWarState,
    PersistedCombatCrimewatchState, PersistedCrimeWatchState, PersistedFwState,
    PersistedInsurgencyState, PersistedSovState, PersistedWarState,
};
use crate::types::*;
use eve_sde::SolarSystemId;
use rusqlite::{Connection, Transaction};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[cfg(test)]
pub(crate) const LEGACY_D6_CORP_CRIMEWATCH_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS d6_corp_crimewatch_kv (
    store_key TEXT PRIMARY KEY,
    payload_json TEXT NOT NULL
);
"#;

pub(crate) const KEY_CORP_GOV: &str = "corp_governance_domain";
pub(crate) const KEY_CRIMEWATCH: &str = "crimewatch_domain";
pub(crate) const KEY_WARFARE: &str = "warfare_domain";

/// Deterministic JSON serializer for `HashSet<i64>` values in `structure_docking_acl_denied`.
struct SortedI64SetRef<'a>(&'a HashSet<i64>);

impl Serialize for SortedI64SetRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut sorted: Vec<i64> = self.0.iter().copied().collect();
        sorted.sort_unstable();
        let mut seq = serializer.serialize_seq(Some(sorted.len()))?;
        for id in sorted {
            seq.serialize_element(&id)?;
        }
        seq.end()
    }
}

trait PrefixedDomainFields: Sized {
    fn collect_prefixed_diffs(
        &self,
        after: &Self,
        domain: &'static str,
        prefix: &str,
        out: &mut Vec<DomainRowChange>,
    );

    fn push_prefixed_sync(
        &self,
        state: &mut DomainSyncState,
        prefix: &str,
    ) -> Result<(), String>;

    fn apply_prefixed_row(
        &mut self,
        sub_key: &str,
        raw_val: &str,
        domain: &'static str,
        full_raw_key: &str,
    ) -> Result<(), String>;
}

macro_rules! impl_decomposed_domain_struct {
    (
        $ty:ty,
        maps: [ $($map_f:ident),* $(,)? ],
        sorted_set_maps: [ $($ssmap_f:ident),* $(,)? ],
        sets: [ $($set_f:ident),* $(,)? ],
        scalars: [ $($scalar_f:ident),* $(,)? ],
        substructs: [ $(($sub_f:ident, $sub_prefix:literal)),* $(,)? ]
    ) => {
        impl PrefixedDomainFields for $ty {
            fn collect_prefixed_diffs(
                &self,
                after: &Self,
                domain: &'static str,
                prefix: &str,
                out: &mut Vec<DomainRowChange>,
            ) {
                let def = Self::default();
                let _ = &def;
                $(
                    if !(self.$map_f.is_empty() && after.$map_f.is_empty()) {
                        diff_map_with_mapped(
                            domain,
                            prefix,
                            stringify!($map_f),
                            self.$map_f.len(),
                            after.$map_f.iter(),
                            |k| self.$map_f.get(k),
                            self.$map_f.keys(),
                            |k| after.$map_f.contains_key(k),
                            |v| v,
                            out,
                        );
                    }
                )*
                $(
                    if !(self.$ssmap_f.is_empty() && after.$ssmap_f.is_empty()) {
                        diff_map_with_mapped(
                            domain,
                            prefix,
                            stringify!($ssmap_f),
                            self.$ssmap_f.len(),
                            after.$ssmap_f.iter(),
                            |k| self.$ssmap_f.get(k),
                            self.$ssmap_f.keys(),
                            |k| after.$ssmap_f.contains_key(k),
                            |v| SortedI64SetRef(v),
                            out,
                        );
                    }
                )*
                $(
                    if !(self.$set_f.is_empty() && after.$set_f.is_empty()) {
                        diff_set_with_prefix(
                            domain,
                            prefix,
                            stringify!($set_f),
                            self.$set_f.len(),
                            after.$set_f.iter(),
                            |k| self.$set_f.contains(k),
                            self.$set_f.iter(),
                            |k| after.$set_f.contains(k),
                            out,
                        );
                    }
                )*
                $(
                    if self.$scalar_f != after.$scalar_f {
                        let key = format_domain_field_key(prefix, stringify!($scalar_f));
                        diff_scalar(
                            domain,
                            &key,
                            &self.$scalar_f,
                            &after.$scalar_f,
                            &def.$scalar_f,
                            out,
                        );
                    }
                )*
                $(
                    if self.$sub_f != after.$sub_f {
                        self.$sub_f.collect_prefixed_diffs(
                            &after.$sub_f,
                            domain,
                            $sub_prefix,
                            out,
                        );
                    }
                )*
            }

            fn push_prefixed_sync(
                &self,
                state: &mut DomainSyncState,
                prefix: &str,
            ) -> Result<(), String> {
                let def = Self::default();
                let _ = &def;
                $(
                    state.push_map_mapped(
                        prefix,
                        stringify!($map_f),
                        self.$map_f.iter(),
                        |v| v,
                    )?;
                )*
                $(
                    state.push_map_mapped(
                        prefix,
                        stringify!($ssmap_f),
                        self.$ssmap_f.iter(),
                        |v| SortedI64SetRef(v),
                    )?;
                )*
                $(
                    state.push_set_with_prefix(prefix, stringify!($set_f), self.$set_f.iter())?;
                )*
                $(
                    let scalar_key = format_domain_field_key(prefix, stringify!($scalar_f));
                    state.push_scalar(&scalar_key, &self.$scalar_f, &def.$scalar_f)?;
                )*
                $(
                    self.$sub_f.push_prefixed_sync(state, $sub_prefix)?;
                )*
                Ok(())
            }

            fn apply_prefixed_row(
                &mut self,
                sub_key: &str,
                raw_val: &str,
                domain: &'static str,
                full_raw_key: &str,
            ) -> Result<(), String> {
                $(
                    if let Some(nested_key) = sub_key.strip_prefix($sub_prefix) {
                        return self.$sub_f.apply_prefixed_row(
                            nested_key,
                            raw_val,
                            domain,
                            full_raw_key,
                        );
                    }
                )*
                let split = match sub_key.split_once('/') {
                    Some((f, sub)) => (f, Some(sub)),
                    None => (sub_key, None),
                };
                match split {
                    $(
                        (stringify!($map_f), Some(sub)) => {
                            let (k, v) = decode_map_entry(sub, raw_val)?;
                            self.$map_f.insert(k, v);
                            Ok(())
                        }
                    )*
                    $(
                        (stringify!($ssmap_f), Some(sub)) => {
                            let (k, v) = decode_map_entry(sub, raw_val)?;
                            self.$ssmap_f.insert(k, v);
                            Ok(())
                        }
                    )*
                    $(
                        (stringify!($set_f), Some(sub)) => {
                            let k = decode_set_entry(sub, raw_val)?;
                            self.$set_f.insert(k);
                            Ok(())
                        }
                    )*
                    $(
                        (stringify!($scalar_f), None) => {
                            self.$scalar_f = from_checked_domain_json(raw_val)?;
                            Ok(())
                        }
                    )*
                    _ => Err(format!("unknown domain_rows key '{full_raw_key}' for {domain}")),
                }
            }
        }

        impl LegacyMerge for $ty {
            fn merge_legacy(&mut self, legacy: Self) {
                let def = Self::default();
                let _ = &def;
                $(
                    for (k, v) in legacy.$map_f {
                        self.$map_f.entry(k).or_insert(v);
                    }
                )*
                $(
                    for (k, v) in legacy.$ssmap_f {
                        self.$ssmap_f.entry(k).or_insert(v);
                    }
                )*
                $(
                    for k in legacy.$set_f {
                        self.$set_f.insert(k);
                    }
                )*
                $(
                    if self.$scalar_f == def.$scalar_f && legacy.$scalar_f != def.$scalar_f {
                        self.$scalar_f = legacy.$scalar_f;
                    }
                )*
                $(
                    self.$sub_f.merge_legacy(legacy.$sub_f);
                )*
            }
        }
    };
}

macro_rules! impl_persist_for_decomposed_domain {
    ($ty:ty) => {
        impl Persist for $ty {
            fn collect_diff_ops(
                &self,
                after: &Self,
                domain: &'static str,
                out: &mut Vec<DomainRowChange>,
            ) {
                self.collect_prefixed_diffs(after, domain, "", out);
            }

            fn sync_full(&self, tx: &Transaction<'_>, domain: &'static str) -> Result<bool, String> {
                let mut state = DomainSyncState::load(tx, domain)?;
                self.push_prefixed_sync(&mut state, "")?;
                state.finish(tx)
            }

            fn load_domain(
                conn: &Connection,
                domain: &'static str,
                ctx: &mut super::quarantine::LoadContext,
            ) -> Result<Self, String> {
                let rows = fetch_domain_row_entries(conn, domain)?;
                let mut out = Self::default();
                for (rowid, raw_key, raw_val) in rows {
                    if let Err(err) = out.apply_prefixed_row(&raw_key, &raw_val, domain, &raw_key)
                    {
                        ctx.record_bad_row("domain_rows", domain, raw_key, raw_val, err, Some(rowid))?;
                    }
                }
                Ok(out)
            }
        }
    };
}

impl_decomposed_domain_struct!(
    PersistedCombatCrimewatchState,
    maps: [
        sec_status_transactions,
        safety_levels,
        drone_settings,
        limited_engagements,
        npc_timers,
        disapproval_flags,
        duel_challenges,
        corp_aggression_settings,
        player_bounty_pool,
        bounty_contributions,
        bounty_hunter_claimed,
        kill_right_expires_unix,
        kill_right_restricted_to,
        detailed_killmails,
        pending_bounty_ticks,
        bounty_tick_next_flush_unix,
        pending_bounty_tick_started_unix,
    ],
    sorted_set_maps: [],
    sets: [],
    scalars: [extended_runtime_json],
    substructs: []
);

impl_decomposed_domain_struct!(
    PersistedAllianceWarState,
    maps: [alliances, project_meta, project_details],
    sorted_set_maps: [],
    sets: [],
    scalars: [],
    substructs: []
);

impl_decomposed_domain_struct!(
    PersistedWarState,
    maps: [wars, mutual_invites, blocked_mutual_invites],
    sorted_set_maps: [],
    sets: [],
    scalars: [negotiations, next_negotiation_id, peace_treaties],
    substructs: []
);

impl_decomposed_domain_struct!(
    PersistedFwState,
    maps: [
        system_victory_points,
        system_advantage,
        battlefield_zones,
        battlefield_participants,
        battlefield_pilot_seconds,
        battlefield_waves,
        battlefield_respawn_unix,
        fw_complexes,
        ihub_lp_donations,
        ihub_tiers,
        ihub_vulnerable,
        ihub_vulnerable_since_unix,
        last_ihub_attacked_unix,
        pending_downtime_captures,
        corp_war_factions,
        complex_occupants,
        supply_sites,
        direct_enlisted_chars,
        complex_last_step_unix,
        complex_capturing_faction,
        complex_seconds_elapsed,
        complex_guards_cleared,
        complex_tier_cooldowns,
        battlefield_npc_waves,
        historical_char_totals,
        historical_corp_totals,
        historical_fac_totals,
        historical_ally_totals,
    ],
    sorted_set_maps: [],
    sets: [],
    scalars: [battlefield_instances, stat_events],
    substructs: []
);

impl_decomposed_domain_struct!(
    PersistedSovState,
    maps: [
        fuel_access_groups,
        hacked_hubs,
        skyhook_reagents,
        skyhook_hackable,
        skyhook_active,
        skyhook_planets,
        skyhook_theft_windows,
        sov_hubs,
        mercenary_dens,
        mercenary_activities,
        skyhook_reagent_bays,
        mercenary_den_reinforcement,
        mercenary_den_tax_pct,
        command_nodes,
        sov_npc_kills,
        sov_mined_units,
        adm_vulnerability_windows,
        skyhook_maturity_rem,
        mercenary_den_evo_rem,
        skyhook_last_harvest_unix,
        skyhook_silo_links,
        entosis_capture_progress,
        tenebrex_spool_ready_unix,
        tenebrex_spool_notified,
        sov_claimed_since_unix,
        mto_site_balls,
        command_node_scores,
        sov_dev_decay_last_unix,
        command_node_regen_last_unix,
    ],
    sorted_set_maps: [],
    sets: [sov_upgrade_combat_npcs],
    scalars: [next_mercenary_activity_seq],
    substructs: []
);

impl_decomposed_domain_struct!(
    PersistedInsurgencyState,
    maps: [
        campaigns,
        pirate_ambition_modifier,
        empire_ambition_modifier,
        fob_armor_reinforced_until,
        campaign_contributions,
        campaign_outcomes,
        surrender_ihub_vulnerable_unix,
    ],
    sorted_set_maps: [],
    sets: [active_state_notified],
    scalars: [insurgency_instances, ice_heist_instances],
    substructs: []
);

#[derive(Debug, Clone, Default, PartialEq)]
struct CorpGovernanceDomain {
    corp_governance: HashMap<i64, CorpGovState>,
    employment_history: HashMap<i64, Vec<(i64, u64, Option<u64>)>>,
    corp_hangar_divisions: HashMap<i64, HashMap<u8, String>>,
    corp_alliance_history: HashMap<i64, Vec<CorpAllianceHistoryEntry>>,
    corp_container_logs: HashMap<i64, Vec<CorpContainerLogEntry>>,
    corp_medals: HashMap<i64, Vec<CorpMedalEntry>>,
    corp_issued_medals: HashMap<i64, Vec<CorpIssuedMedalEntry>>,
    corp_member_limits: HashMap<i64, i64>,
    corp_titles: HashMap<i64, Vec<CorpTitleEntry>>,
    corp_member_titles: HashMap<(i64, i64), Vec<i64>>,
    corp_role_assignments: HashMap<(i64, i64), CorpMemberRoleAssignment>,
    corp_role_history: HashMap<i64, Vec<CorpRoleHistoryEntry>>,
    corp_shareholders: HashMap<i64, Vec<CorpShareholderEntry>>,
    char_medals: HashMap<i64, Vec<CharacterMedalRecord>>,
    char_scoped_roles: HashMap<i64, CharacterScopedRolesRecord>,
    corp_vote_cases: HashMap<i64, CorpVoteCaseRecord>,
    corp_vote_ballots: HashMap<(i64, i64), CorpVoteBallotRecord>,
}

impl_decomposed_domain_struct!(
    CorpGovernanceDomain,
    maps: [
        corp_governance,
        employment_history,
        corp_hangar_divisions,
        corp_alliance_history,
        corp_container_logs,
        corp_medals,
        corp_issued_medals,
        corp_member_limits,
        corp_titles,
        corp_member_titles,
        corp_role_assignments,
        corp_role_history,
        corp_shareholders,
        char_medals,
        char_scoped_roles,
        corp_vote_cases,
        corp_vote_ballots,
    ],
    sorted_set_maps: [],
    sets: [],
    scalars: [],
    substructs: []
);
impl_persist_for_decomposed_domain!(CorpGovernanceDomain);

#[derive(Debug, Clone, Default, PartialEq)]
struct CrimewatchDomain {
    crimewatch_states: HashMap<i64, PersistedCrimeWatchState>,
    crimewatch_aggressions: HashMap<(i64, i64), PersistedAggressionRecord>,
    wh_polarizations: HashMap<(i64, i64), u64>,
    zarzakh_gate_locks: HashMap<i64, (i64, u64)>,
    structure_docking_acl_denied: HashMap<i64, HashSet<i64>>,
    combat_crimewatch_state: PersistedCombatCrimewatchState,
}

impl_decomposed_domain_struct!(
    CrimewatchDomain,
    maps: [
        crimewatch_states,
        crimewatch_aggressions,
        wh_polarizations,
        zarzakh_gate_locks,
    ],
    sorted_set_maps: [structure_docking_acl_denied],
    sets: [],
    scalars: [],
    substructs: [(combat_crimewatch_state, "combat_crimewatch_state.")]
);
impl_persist_for_decomposed_domain!(CrimewatchDomain);

#[derive(Debug, Clone, Default, PartialEq)]
struct WarfareDomain {
    sov_prospecting_levels: HashMap<SolarSystemId, u32>,
    sov_campaign_scores: HashMap<i64, SovCampaignScoreRecord>,
    fw_character_stats: HashMap<i64, FwCharacterStatsRecord>,
    fw_corp_enlisted_on: HashMap<i64, u64>,
    fw_system_occupations: HashMap<SolarSystemId, FwSystemOccupationRecord>,
    war_lifecycle_meta: HashMap<i64, WarLifecycleMeta>,
    alliance_lifecycle_meta: HashMap<i64, AllianceLifecycleMeta>,
    fw_wars: Vec<(i64, i64)>,
    alliance_war_state: PersistedAllianceWarState,
    war_state: PersistedWarState,
    fw_runtime: PersistedFwState,
    sov_runtime: PersistedSovState,
    insurgency_runtime: PersistedInsurgencyState,
}

impl_decomposed_domain_struct!(
    WarfareDomain,
    maps: [
        sov_prospecting_levels,
        sov_campaign_scores,
        fw_character_stats,
        fw_corp_enlisted_on,
        fw_system_occupations,
        war_lifecycle_meta,
        alliance_lifecycle_meta,
    ],
    sorted_set_maps: [],
    sets: [],
    scalars: [fw_wars],
    substructs: [
        (alliance_war_state, "alliance_war_state."),
        (war_state, "war_state."),
        (fw_runtime, "fw_runtime."),
        (sov_runtime, "sov_runtime."),
        (insurgency_runtime, "insurgency_runtime."),
    ]
);
impl_persist_for_decomposed_domain!(WarfareDomain);

// Single canonical field-mapping specs shared by zero-clone diff, sync, and staged load/rollback.
macro_rules! for_each_corp_governance_fields {
    ($m:ident) => {
        $m!(
            maps: [
                corp_governance,
                employment_history,
                corp_hangar_divisions,
                corp_alliance_history,
                corp_container_logs,
                corp_medals,
                corp_issued_medals,
                corp_member_limits,
                corp_titles,
                corp_member_titles,
                corp_role_assignments,
                corp_role_history,
                corp_shareholders,
                char_medals,
                char_scoped_roles,
                corp_vote_cases,
                corp_vote_ballots,
            ],
            sorted_set_maps: [],
            scalars: [],
            substructs: []
        )
    };
}

macro_rules! for_each_crimewatch_fields {
    ($m:ident) => {
        $m!(
            maps: [
                crimewatch_states,
                crimewatch_aggressions,
                wh_polarizations,
                zarzakh_gate_locks,
            ],
            sorted_set_maps: [structure_docking_acl_denied],
            scalars: [],
            substructs: [(combat_crimewatch_state, "combat_crimewatch_state.")]
        )
    };
}

macro_rules! for_each_warfare_fields {
    ($m:ident) => {
        $m!(
            maps: [
                sov_prospecting_levels,
                sov_campaign_scores,
                fw_character_stats,
                fw_corp_enlisted_on,
                fw_system_occupations,
                war_lifecycle_meta,
                alliance_lifecycle_meta,
            ],
            sorted_set_maps: [],
            scalars: [fw_wars],
            substructs: [
                (alliance_war_state, "alliance_war_state."),
                (war_state, "war_state."),
                (fw_runtime, "fw_runtime."),
                (sov_runtime, "sov_runtime."),
                (insurgency_runtime, "insurgency_runtime."),
            ]
        )
    };
}

macro_rules! collect_snapshot_domain_diffs {
    (
        $before:expr,
        $after:expr,
        $domain:expr,
        $out:expr,
        maps: [ $($map_f:ident),* $(,)? ],
        sorted_set_maps: [ $($ssmap_f:ident),* $(,)? ],
        scalars: [ $($scalar_f:ident),* $(,)? ],
        substructs: [ $(($sub_f:ident, $sub_prefix:literal)),* $(,)? ]
    ) => {{
        $(
            if !$before.$map_f.ptr_eq(&$after.$map_f)
                && !($before.$map_f.is_empty() && $after.$map_f.is_empty())
            {
                diff_map_with_mapped(
                    $domain,
                    "",
                    stringify!($map_f),
                    $before.$map_f.len(),
                    $after.$map_f.iter(),
                    |k| $before.$map_f.get(k),
                    $before.$map_f.keys(),
                    |k| $after.$map_f.contains_key(k),
                    |v| v,
                    $out,
                );
            }
        )*
        $(
            if !$before.$ssmap_f.ptr_eq(&$after.$ssmap_f)
                && !($before.$ssmap_f.is_empty() && $after.$ssmap_f.is_empty())
            {
                diff_map_with_mapped(
                    $domain,
                    "",
                    stringify!($ssmap_f),
                    $before.$ssmap_f.len(),
                    $after.$ssmap_f.iter(),
                    |k| $before.$ssmap_f.get(k),
                    $before.$ssmap_f.keys(),
                    |k| $after.$ssmap_f.contains_key(k),
                    |v| SortedI64SetRef(v),
                    $out,
                );
            }
        )*
        $(
            if !$before.$scalar_f.ptr_eq(&$after.$scalar_f)
                && *$before.$scalar_f != *$after.$scalar_f
            {
                let def = Default::default();
                diff_scalar(
                    $domain,
                    stringify!($scalar_f),
                    &*$before.$scalar_f,
                    &*$after.$scalar_f,
                    &def,
                    $out,
                );
            }
        )*
        $(
            if !$before.$sub_f.ptr_eq(&$after.$sub_f)
                && *$before.$sub_f != *$after.$sub_f
            {
                $before.$sub_f.collect_prefixed_diffs(
                    &$after.$sub_f,
                    $domain,
                    $sub_prefix,
                    $out,
                );
            }
        )*
    }};
}

macro_rules! sync_snapshot_domain {
    (
        $tx:expr,
        $snap:expr,
        $domain:expr,
        maps: [ $($map_f:ident),* $(,)? ],
        sorted_set_maps: [ $($ssmap_f:ident),* $(,)? ],
        scalars: [ $($scalar_f:ident),* $(,)? ],
        substructs: [ $(($sub_f:ident, $sub_prefix:literal)),* $(,)? ]
    ) => {{
        let mut state = DomainSyncState::load($tx, $domain)?;
        $(
            state.push_map_mapped("", stringify!($map_f), $snap.$map_f.iter(), |v| v)?;
        )*
        $(
            state.push_map_mapped(
                "",
                stringify!($ssmap_f),
                $snap.$ssmap_f.iter(),
                |v| SortedI64SetRef(v),
            )?;
        )*
        $(
            let def = Default::default();
            state.push_scalar(stringify!($scalar_f), &*$snap.$scalar_f, &def)?;
        )*
        $(
            $snap.$sub_f.push_prefixed_sync(&mut state, $sub_prefix)?;
        )*
        state.finish($tx)
    }};
}

/// Collects zero-clone `domain_rows` diffs for all 3 Family 4 domains (`corp_crimewatch.*`).
pub(crate) fn collect_corp_crimewatch_domain_row_diffs(
    before: &WorldStateSnapshot,
    after: &WorldStateSnapshot,
    out: &mut Vec<DomainRowChange>,
) {
    macro_rules! diff_corp {
        ($($rest:tt)*) => {
            collect_snapshot_domain_diffs!(before, after, DOMAIN_CORP_GOVERNANCE, out, $($rest)*)
        };
    }
    macro_rules! diff_cw {
        ($($rest:tt)*) => {
            collect_snapshot_domain_diffs!(before, after, DOMAIN_CRIMEWATCH, out, $($rest)*)
        };
    }
    macro_rules! diff_war {
        ($($rest:tt)*) => {
            collect_snapshot_domain_diffs!(before, after, DOMAIN_WARFARE, out, $($rest)*)
        };
    }
    for_each_corp_governance_fields!(diff_corp);
    for_each_crimewatch_fields!(diff_cw);
    for_each_warfare_fields!(diff_war);
}

/// Synchronizes all 3 Family 4 domains (`corp_crimewatch.*`) into `domain_rows` with zero clones.
pub(crate) fn sync_corp_crimewatch_domain_rows(
    tx: &Transaction<'_>,
    snap: &WorldStateSnapshot,
) -> Result<bool, String> {
    let mut dirty = false;
    macro_rules! sync_corp {
        ($($rest:tt)*) => {
            sync_snapshot_domain!(tx, snap, DOMAIN_CORP_GOVERNANCE, $($rest)*)?
        };
    }
    macro_rules! sync_cw {
        ($($rest:tt)*) => {
            sync_snapshot_domain!(tx, snap, DOMAIN_CRIMEWATCH, $($rest)*)?
        };
    }
    macro_rules! sync_war {
        ($($rest:tt)*) => {
            sync_snapshot_domain!(tx, snap, DOMAIN_WARFARE, $($rest)*)?
        };
    }
    dirty |= for_each_corp_governance_fields!(sync_corp);
    dirty |= for_each_crimewatch_fields!(sync_cw);
    dirty |= for_each_warfare_fields!(sync_war);
    Ok(dirty)
}

// Legacy `d6_corp_crimewatch_kv` payload decoders used strictly during one-time migration.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(test, derive(Serialize))]
#[serde(default)]
struct CorpGovernanceDomainPayload {
    #[serde(default)]
    corp_governance: HashMap<i64, CorpGovState>,
    #[serde(default)]
    employment_history: HashMap<i64, Vec<(i64, u64, Option<u64>)>>,
    #[serde(default)]
    corp_hangar_divisions: HashMap<i64, HashMap<u8, String>>,
    #[serde(default)]
    corp_alliance_history: HashMap<i64, Vec<CorpAllianceHistoryEntry>>,
    #[serde(default)]
    corp_container_logs: HashMap<i64, Vec<CorpContainerLogEntry>>,
    #[serde(default)]
    corp_medals: HashMap<i64, Vec<CorpMedalEntry>>,
    #[serde(default)]
    corp_issued_medals: HashMap<i64, Vec<CorpIssuedMedalEntry>>,
    #[serde(default)]
    corp_member_limits: HashMap<i64, i64>,
    #[serde(default)]
    corp_titles: HashMap<i64, Vec<CorpTitleEntry>>,
    #[serde(default, with = "tuple_map_serde")]
    corp_member_titles: HashMap<(i64, i64), Vec<i64>>,
    #[serde(default, with = "tuple_map_serde")]
    corp_role_assignments: HashMap<(i64, i64), CorpMemberRoleAssignment>,
    #[serde(default)]
    corp_role_history: HashMap<i64, Vec<CorpRoleHistoryEntry>>,
    #[serde(default)]
    corp_shareholders: HashMap<i64, Vec<CorpShareholderEntry>>,
    #[serde(default)]
    char_medals: HashMap<i64, Vec<CharacterMedalRecord>>,
    #[serde(default)]
    char_scoped_roles: HashMap<i64, CharacterScopedRolesRecord>,
    #[serde(default)]
    corp_vote_cases: HashMap<i64, CorpVoteCaseRecord>,
    #[serde(default, with = "tuple_map_serde")]
    corp_vote_ballots: HashMap<(i64, i64), CorpVoteBallotRecord>,
}

impl From<CorpGovernanceDomainPayload> for CorpGovernanceDomain {
    fn from(p: CorpGovernanceDomainPayload) -> Self {
        Self {
            corp_governance: p.corp_governance,
            employment_history: p.employment_history,
            corp_hangar_divisions: p.corp_hangar_divisions,
            corp_alliance_history: p.corp_alliance_history,
            corp_container_logs: p.corp_container_logs,
            corp_medals: p.corp_medals,
            corp_issued_medals: p.corp_issued_medals,
            corp_member_limits: p.corp_member_limits,
            corp_titles: p.corp_titles,
            corp_member_titles: p.corp_member_titles,
            corp_role_assignments: p.corp_role_assignments,
            corp_role_history: p.corp_role_history,
            corp_shareholders: p.corp_shareholders,
            char_medals: p.char_medals,
            char_scoped_roles: p.char_scoped_roles,
            corp_vote_cases: p.corp_vote_cases,
            corp_vote_ballots: p.corp_vote_ballots,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(test, derive(Serialize))]
#[serde(default)]
struct CrimewatchDomainPayload {
    #[serde(default)]
    crimewatch_states: HashMap<i64, PersistedCrimeWatchState>,
    #[serde(default, with = "tuple_map_serde")]
    crimewatch_aggressions: HashMap<(i64, i64), PersistedAggressionRecord>,
    #[serde(default)]
    combat_crimewatch_state: PersistedCombatCrimewatchState,
    #[serde(default, with = "tuple_map_serde")]
    wh_polarizations: HashMap<(i64, i64), u64>,
    #[serde(default)]
    zarzakh_gate_locks: HashMap<i64, (i64, u64)>,
    #[serde(default)]
    structure_docking_acl_denied: HashMap<i64, Vec<i64>>,
}

impl From<CrimewatchDomainPayload> for CrimewatchDomain {
    fn from(p: CrimewatchDomainPayload) -> Self {
        Self {
            crimewatch_states: p.crimewatch_states,
            crimewatch_aggressions: p.crimewatch_aggressions,
            wh_polarizations: p.wh_polarizations,
            zarzakh_gate_locks: p.zarzakh_gate_locks,
            structure_docking_acl_denied: p
                .structure_docking_acl_denied
                .into_iter()
                .map(|(sid, ids)| (sid, ids.into_iter().collect()))
                .collect(),
            combat_crimewatch_state: p.combat_crimewatch_state,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(test, derive(Serialize))]
#[serde(default)]
struct WarfareDomainPayload {
    #[serde(default)]
    alliance_war_state: PersistedAllianceWarState,
    #[serde(default)]
    war_state: PersistedWarState,
    #[serde(default)]
    fw_runtime: PersistedFwState,
    #[serde(default)]
    sov_runtime: PersistedSovState,
    #[serde(default)]
    insurgency_runtime: PersistedInsurgencyState,
    #[serde(default)]
    sov_prospecting_levels: HashMap<SolarSystemId, u32>,
    #[serde(default)]
    sov_campaign_scores: HashMap<i64, SovCampaignScoreRecord>,
    #[serde(default)]
    fw_character_stats: HashMap<i64, FwCharacterStatsRecord>,
    #[serde(default)]
    fw_corp_enlisted_on: HashMap<i64, u64>,
    #[serde(default)]
    fw_system_occupations: HashMap<SolarSystemId, FwSystemOccupationRecord>,
    #[serde(default)]
    fw_wars: Vec<(i64, i64)>,
    #[serde(default)]
    war_lifecycle_meta: HashMap<i64, WarLifecycleMeta>,
    #[serde(default)]
    alliance_lifecycle_meta: HashMap<i64, AllianceLifecycleMeta>,
}

impl From<WarfareDomainPayload> for WarfareDomain {
    fn from(p: WarfareDomainPayload) -> Self {
        Self {
            sov_prospecting_levels: p.sov_prospecting_levels,
            sov_campaign_scores: p.sov_campaign_scores,
            fw_character_stats: p.fw_character_stats,
            fw_corp_enlisted_on: p.fw_corp_enlisted_on,
            fw_system_occupations: p.fw_system_occupations,
            war_lifecycle_meta: p.war_lifecycle_meta,
            alliance_lifecycle_meta: p.alliance_lifecycle_meta,
            fw_wars: p.fw_wars,
            alliance_war_state: p.alliance_war_state,
            war_state: p.war_state,
            fw_runtime: p.fw_runtime,
            sov_runtime: p.sov_runtime,
            insurgency_runtime: p.insurgency_runtime,
        }
    }
}

#[derive(Default)]
struct StagedCorpCrimewatch {
    corp_governance: CorpGovernanceDomain,
    crimewatch: CrimewatchDomain,
    warfare: WarfareDomain,
}

impl StagedCorpCrimewatch {
    fn load_from_domain_rows(
        conn: &Connection,
        ctx: &mut super::quarantine::LoadContext,
    ) -> Result<Self, String> {
        Ok(Self {
            corp_governance: CorpGovernanceDomain::load_domain(conn, DOMAIN_CORP_GOVERNANCE, ctx)?,
            crimewatch: CrimewatchDomain::load_domain(conn, DOMAIN_CRIMEWATCH, ctx)?,
            warfare: WarfareDomain::load_domain(conn, DOMAIN_WARFARE, ctx)?,
        })
    }

    fn merge_legacy_row(&mut self, store_key: &str, raw_json: &str) -> Result<(), String> {
        match store_key {
            KEY_CORP_GOV => {
                let payload = from_checked_domain_json::<CorpGovernanceDomainPayload>(raw_json)?;
                self.corp_governance.merge_legacy(payload.into());
                Ok(())
            }
            KEY_CRIMEWATCH => {
                let payload = from_checked_domain_json::<CrimewatchDomainPayload>(raw_json)?;
                self.crimewatch.merge_legacy(payload.into());
                Ok(())
            }
            KEY_WARFARE => {
                let payload = from_checked_domain_json::<WarfareDomainPayload>(raw_json)?;
                self.warfare.merge_legacy(payload.into());
                Ok(())
            }
            _ => from_checked_domain_json::<serde_json::Value>(raw_json).map(|_| ()),
        }
    }

    fn apply_to(self, snap: &mut WorldStateSnapshot) {
        macro_rules! assign_fields {
            (
                $src:expr,
                maps: [ $($map_f:ident),* $(,)? ],
                sorted_set_maps: [ $($ssmap_f:ident),* $(,)? ],
                scalars: [ $($scalar_f:ident),* $(,)? ],
                substructs: [ $(($sub_f:ident, $_sub_prefix:literal)),* $(,)? ]
            ) => {{
                $(snap.$map_f = $src.$map_f.into();)*
                $(snap.$ssmap_f = $src.$ssmap_f.into();)*
                $(snap.$scalar_f = $src.$scalar_f.into();)*
                $(snap.$sub_f = $src.$sub_f.into();)*
            }};
        }
        macro_rules! apply_corp {
            ($($rest:tt)*) => {
                assign_fields!(self.corp_governance, $($rest)*);
            };
        }
        macro_rules! apply_cw {
            ($($rest:tt)*) => {
                assign_fields!(self.crimewatch, $($rest)*);
            };
        }
        macro_rules! apply_war {
            ($($rest:tt)*) => {
                assign_fields!(self.warfare, $($rest)*);
            };
        }
        for_each_corp_governance_fields!(apply_corp);
        for_each_crimewatch_fields!(apply_cw);
        for_each_warfare_fields!(apply_war);
    }
}

fn stage_corp_crimewatch_rows(
    conn: &Connection,
    ctx: &mut super::quarantine::LoadContext,
) -> Result<(StagedCorpCrimewatch, bool), String> {
    let mut staged = StagedCorpCrimewatch::load_from_domain_rows(conn, ctx)?;
    let had_legacy_kv = table_exists(conn, "d6_corp_crimewatch_kv")?;

    if had_legacy_kv {
        let mut stmt = conn
            .prepare("SELECT rowid, store_key, payload_json FROM d6_corp_crimewatch_kv")
            .map_err(|e| format!("legacy d6_corp_crimewatch_kv prepare: {e}"))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| format!("legacy d6_corp_crimewatch_kv query: {e}"))?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = super::quarantine::row_col_string(r, 1);
            let raw_payload = super::quarantine::row_col_string(r, 2);
            let parse_res = r
                .get::<_, String>(2)
                .map_err(|e| e.to_string())
                .and_then(|s| staged.merge_legacy_row(&key, &s));
            if let Err(err) = parse_res {
                ctx.record_bad_row(
                    "d6_corp_crimewatch_kv",
                    "d6_corp_crimewatch_kv",
                    key,
                    raw_payload,
                    err,
                    rowid,
                )?;
            }
        }
    }

    // Drain any historical duplicate entries for `corp_container_logs` / `wh_polarizations`
    // that earlier builds also mirrored into `world_kv_blobs` without overwriting newer rows.
    let mut had_legacy_world_blobs = false;
    if table_exists(conn, "world_kv_blobs")? {
        let mut stmt = conn
            .prepare(
                "SELECT rowid, blob_key, blob_json FROM world_kv_blobs WHERE blob_key IN ('corp_container_logs', 'wh_polarizations')",
            )
            .map_err(|e| format!("legacy world_kv_blobs corp_crimewatch prepare: {e}"))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| format!("legacy world_kv_blobs corp_crimewatch query: {e}"))?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            had_legacy_world_blobs = true;
            let rowid = r.get::<_, i64>(0).ok();
            let key = super::quarantine::row_col_string(r, 1);
            let raw_payload = super::quarantine::row_col_string(r, 2);
            let parse_res = r
                .get::<_, String>(2)
                .map_err(|e| e.to_string())
                .and_then(|s| match key.as_str() {
                    "corp_container_logs" => {
                        let entries = from_checked_domain_json::<
                            Vec<(i64, Vec<CorpContainerLogEntry>)>,
                        >(&s)?;
                        for (cid, logs) in entries {
                            staged
                                .corp_governance
                                .corp_container_logs
                                .entry(cid)
                                .or_insert(logs);
                        }
                        Ok(())
                    }
                    "wh_polarizations" => {
                        let entries =
                            from_checked_domain_json::<Vec<((i64, i64), u64)>>(&s)?;
                        for (pair, count) in entries {
                            staged.crimewatch.wh_polarizations.entry(pair).or_insert(count);
                        }
                        Ok(())
                    }
                    _ => Ok(()),
                });
            if let Err(err) = parse_res {
                ctx.record_bad_row(
                    "world_kv_blobs",
                    "world_kv_blobs",
                    key,
                    raw_payload,
                    err,
                    rowid,
                )?;
            }
        }
    }

    Ok((staged, had_legacy_kv || had_legacy_world_blobs))
}

/// Drops retired legacy Family 4 persistence artifacts once migrated rows are flushed into `domain_rows`.
pub(crate) fn drop_legacy_corp_crimewatch_tables(tx: &Transaction<'_>) -> Result<(), String> {
    tx.execute_batch("DROP TABLE IF EXISTS d6_corp_crimewatch_kv;")
        .map_err(|e| format!("drop legacy d6_corp_crimewatch_kv table: {e}"))?;
    if table_exists(tx, "world_kv_blobs")? {
        tx.execute(
            "DELETE FROM world_kv_blobs WHERE blob_key IN ('corp_container_logs', 'wh_polarizations')",
            [],
        )
        .map_err(|e| format!("delete legacy world_kv_blobs corp_crimewatch keys: {e}"))?;
    }
    Ok(())
}

/// Loads all 3 Family 4 domains (`corp_crimewatch.*` in `domain_rows`, plus any legacy
/// `d6_corp_crimewatch_kv` / duplicate `world_kv_blobs` rows), staging all parses before
/// committing to `snap`.
///
/// Returns `true` when legacy Family 4 rows/tables existed and await cleanup once committed.
pub fn load_corp_crimewatch_domain_rows(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
    ctx: &mut super::quarantine::LoadContext,
) -> Result<bool, String> {
    let (staged, had_legacy) = stage_corp_crimewatch_rows(conn, ctx)?;
    staged.apply_to(snap);
    Ok(had_legacy)
}

fn rollback_corp_crimewatch_inner(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
) -> Result<(), String> {
    let has_domain_rows = table_exists(conn, "domain_rows")?;
    let has_legacy_table = table_exists(conn, "d6_corp_crimewatch_kv")?;
    if !has_domain_rows && !has_legacy_table {
        return Err("rollback corp_crimewatch missing domain_rows table".into());
    }
    let mut ctx = super::quarantine::LoadContext::new(false);
    let (staged, _) = stage_corp_crimewatch_rows(conn, &mut ctx)?;
    staged.apply_to(snap);
    Ok(())
}

/// Restores all 3 Family 4 domains from SQLite into temporaries, mutating `snap` only
/// after every query and checked JSON parse succeeds.
pub fn rollback_corp_crimewatch(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
) -> Result<(), String> {
    if let Err(err) = rollback_corp_crimewatch_inner(conn, snap) {
        crate::persistence::record_persist_failure();
        tracing::error!("rollback_corp_crimewatch failed closed without mutating snapshot: {err}");
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
#[path = "corp_crimewatch_store_unit_tests.rs"]
mod corp_crimewatch_store_unit_tests;
#[cfg(test)]
#[path = "corp_crimewatch_store_edges_tests.rs"]
mod corp_crimewatch_store_edges_tests;
