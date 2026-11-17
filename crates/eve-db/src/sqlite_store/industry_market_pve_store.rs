//! Industry, moon mining, PI, market/wallet, PvE, ship fitting, skill, starbase,
//! killmail, structure, corp-project, loyalty-store, space-event, service/quasar blob,
//! insurance, and mining-ledger SQLite persistence (`#916` Family 5).
//!
//! All 34 former `world_kv_blobs` collections now persist as normalized per-entry rows in
//! `domain_rows` across seven `industry_market_pve.*` namespaces, with one-shot startup migration
//! from legacy `world_kv_blobs`, checked finite-float JSON serialization, zero-cold-clone
//! incremental `ChangeSet` diffs, and fail-closed reload/rollback. `char_mining_ledger`
//! (`snap.mining_ledger`) remains in its dedicated relational SQLite table.

use crate::service_domains::{
    record_service_state_blob_limit_violation, validate_service_state_blobs,
};
use crate::sqlite_store::domain_rows::{
    decode_map_entry, decode_set_entry, diff_map_with, diff_scalar, diff_set_with,
    fetch_domain_row_entries, from_checked_domain_json, table_exists, DomainRowChange,
    DomainSyncState, DOMAIN_CORP_STRUCTURE_AND_KILLMAILS, DOMAIN_INDUSTRY_PVE_PI,
    DOMAIN_PVE_AND_SPACE_EVENTS, DOMAIN_QUASAR_STATE_BLOBS, DOMAIN_SERVICE_STATE_BLOBS,
    DOMAIN_SHIP_FITTING_AND_SKILLS, DOMAIN_WALLET_AND_MARKET,
};
use crate::sqlite_store::quarantine::LoadContext;
use crate::types::{
    AutoMoonMinerRecord, CharacterResearchAgentRecord, CorpProjectContributorEntry,
    CorpWalletJournalRecord, CosmicAnomalyRecord, CosmicSignatureRecord, CustomsOfficeRecord,
    IncursionRecord, KillmailDetailRecord, LoyaltyStoreOfferRecord, MarketHistoryDayRecord,
    MarketOrderMeta, MoonExtractionRecord, ObserverMiningRecord,
    PersistedInsuranceContractRecord, PlanetaryColonyLayoutRecord, ShipFitInputsRecord,
    StarbaseDetailSettings, StructureEsiTimerMeta, SystemJumpEvent, SystemKillEvent,
    WalletTransactionRecord, WorldStateSnapshot,
};
use eve_sde::{SolarSystemId, TypeId};
use rusqlite::{params, Connection, Transaction};
use serde::de::DeserializeOwned;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

/// Legacy `0005_d6_industry_market_pve_ship_account` table schema retained strictly for pre-`#916`
/// database migration and edge-case tests.
#[cfg(test)]
pub(crate) const LEGACY_WORLD_KV_BLOBS_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS world_kv_blobs (
    blob_key TEXT PRIMARY KEY,
    blob_json TEXT NOT NULL
);
"#;

macro_rules! for_each_industry_market_pve_domain {
    ($cb:ident) => {
        $cb!(
            industry_pi {
                const_name: DOMAIN_INDUSTRY_PVE_PI,
                maps: [
                    moon_extractions: i64 => MoonExtractionRecord,
                    auto_moon_miners: i64 => AutoMoonMinerRecord,
                    observer_mining_ledger: i64 => Vec<ObserverMiningRecord>,
                    char_research_agents: i64 => Vec<CharacterResearchAgentRecord>,
                    planetary_colony_layouts: (i64, i64) => PlanetaryColonyLayoutRecord,
                    corp_customs_offices: i64 => Vec<CustomsOfficeRecord>,
                    orbital_provision_types: i64 => i64,
                ],
                sets: [],
                scalars: [],
            },
            wallet_market {
                const_name: DOMAIN_WALLET_AND_MARKET,
                maps: [
                    wallet_journal_timestamps: i64 => u64,
                    char_wallet_transactions: i64 => Vec<WalletTransactionRecord>,
                    corp_wallet_journals: (i64, i64) => Vec<CorpWalletJournalRecord>,
                    corp_wallet_transactions: (i64, i64) => Vec<WalletTransactionRecord>,
                    market_order_meta: i64 => MarketOrderMeta,
                    market_history: (u32, TypeId) => Vec<MarketHistoryDayRecord>,
                ],
                sets: [],
                scalars: [],
            },
            pve_space_events {
                const_name: DOMAIN_PVE_AND_SPACE_EVENTS,
                maps: [
                    cosmic_signatures: SolarSystemId => Vec<CosmicSignatureRecord>,
                    cosmic_anomalies: SolarSystemId => Vec<CosmicAnomalyRecord>,
                    incursions: u32 => IncursionRecord,
                    last_mission_decline_times: i64 => u64,
                ],
                sets: [],
                scalars: [
                    system_jump_events: Vec<SystemJumpEvent>,
                    system_kill_events: Vec<SystemKillEvent>,
                ],
            },
            ship_fitting_skills {
                const_name: DOMAIN_SHIP_FITTING_AND_SKILLS,
                maps: [
                    module_online_states: i64 => bool,
                    module_active_charges: i64 => u32,
                    ship_fit_inputs: i64 => ShipFitInputsRecord,
                    weapon_banks: i64 => HashMap<i64, Vec<i64>>,
                ],
                sets: [
                    module_active_states: i64,
                    paused_skill_queues: i64,
                ],
                scalars: [],
            },
            corp_structure_killmails {
                const_name: DOMAIN_CORP_STRUCTURE_AND_KILLMAILS,
                maps: [
                    starbase_settings: i64 => StarbaseDetailSettings,
                    killmail_details: i64 => KillmailDetailRecord,
                    station_office_rental_costs: i64 => f64,
                    structure_esi_timer_meta: i64 => StructureEsiTimerMeta,
                    corp_project_contributors: i64 => Vec<CorpProjectContributorEntry>,
                    loyalty_store_offers: i64 => Vec<LoyaltyStoreOfferRecord>,
                    insurance_contracts: i64 => PersistedInsuranceContractRecord,
                ],
                sets: [],
                scalars: [],
            },
            quasar_state_blobs {
                const_name: DOMAIN_QUASAR_STATE_BLOBS,
                maps: [
                    quasar_state_blobs: String => String,
                ],
                sets: [],
                scalars: [],
            },
        );
    };
}

macro_rules! define_staged_industry_market_pve {
    (
        $(
            $dom_name:ident {
                const_name: $domain_const:ident,
                maps: [ $( $map_f:ident : $mk:ty => $mv:ty ),* $(,)? ],
                sets: [ $( $set_f:ident : $sk:ty ),* $(,)? ],
                scalars: [ $( $scalar_f:ident : $sv:ty ),* $(,)? ],
            }
        ),* $(,)?
    ) => {
        #[derive(Default)]
        struct StagedIndustryMarketPve {
            $(
                $( $map_f: HashMap<$mk, $mv>, )*
                $( $set_f: HashSet<$sk>, )*
                $( $scalar_f: $sv, )*
            )*
            service_state_blobs: HashMap<String, String>,
        }

        trait AssignFrom<S> {
            fn assign_from(&mut self, src: S);
        }

        impl<K, V> AssignFrom<HashMap<K, V>> for HashMap<K, V> {
            fn assign_from(&mut self, src: HashMap<K, V>) {
                *self = src;
            }
        }

        impl<K, V> AssignFrom<HashMap<K, V>> for crate::cow::CowCell<HashMap<K, V>> {
            fn assign_from(&mut self, src: HashMap<K, V>) {
                *self = crate::cow::CowCell::new(src);
            }
        }

        impl<K: Ord + Clone, V: Clone> AssignFrom<HashMap<K, V>> for crate::cow::CowMap<K, V> {
            fn assign_from(&mut self, src: HashMap<K, V>) {
                *self = src.into_iter().collect();
            }
        }

        impl<T> AssignFrom<HashSet<T>> for HashSet<T> {
            fn assign_from(&mut self, src: HashSet<T>) {
                *self = src;
            }
        }

        impl<T> AssignFrom<HashSet<T>> for crate::cow::CowCell<HashSet<T>> {
            fn assign_from(&mut self, src: HashSet<T>) {
                *self = crate::cow::CowCell::new(src);
            }
        }

        impl<T> AssignFrom<Vec<T>> for crate::cow::CowCell<Vec<T>> {
            fn assign_from(&mut self, src: Vec<T>) {
                *self = crate::cow::CowCell::new(src);
            }
        }

        impl StagedIndustryMarketPve {
            fn apply_to(self, snap: &mut WorldStateSnapshot) {
                $(
                    $( AssignFrom::assign_from(&mut snap.$map_f, self.$map_f); )*
                    $( AssignFrom::assign_from(&mut snap.$set_f, self.$set_f); )*
                    $( AssignFrom::assign_from(&mut snap.$scalar_f, self.$scalar_f); )*
                )*
                snap.service_state_blobs = self.service_state_blobs.into();
            }
        }
    };
}

for_each_industry_market_pve_domain!(define_staged_industry_market_pve);

fn map_from_legacy_json<K: DeserializeOwned + Eq + Hash, V: DeserializeOwned>(
    s: &str,
) -> Result<HashMap<K, V>, String> {
    let entries: Vec<(K, V)> = serde_json::from_str(s).map_err(|e| e.to_string())?;
    Ok(entries.into_iter().collect())
}

fn nested_map_from_legacy_json<
    K1: DeserializeOwned + Eq + Hash,
    K2: DeserializeOwned + Eq + Hash,
    V: DeserializeOwned,
>(
    s: &str,
) -> Result<HashMap<K1, HashMap<K2, V>>, String> {
    let outer: Vec<(K1, Vec<(K2, V)>)> = serde_json::from_str(s).map_err(|e| e.to_string())?;
    Ok(outer
        .into_iter()
        .map(|(k1, inner)| (k1, inner.into_iter().collect()))
        .collect())
}

fn set_from_legacy_json<T: DeserializeOwned + Eq + Hash>(s: &str) -> Result<HashSet<T>, String> {
    let items: Vec<T> = serde_json::from_str(s).map_err(|e| e.to_string())?;
    Ok(items.into_iter().collect())
}

fn decode_legacy_blob(
    staged: &mut StagedIndustryMarketPve,
    key: &str,
    js: &str,
) -> Result<bool, String> {
    match key {
        "moon_extractions" => staged.moon_extractions = map_from_legacy_json(js)?,
        "auto_moon_miners" => staged.auto_moon_miners = map_from_legacy_json(js)?,
        "observer_mining_ledger" => staged.observer_mining_ledger = map_from_legacy_json(js)?,
        "char_research_agents" => staged.char_research_agents = map_from_legacy_json(js)?,
        "planetary_colony_layouts" => staged.planetary_colony_layouts = map_from_legacy_json(js)?,
        "corp_customs_offices" => staged.corp_customs_offices = map_from_legacy_json(js)?,
        "orbital_provision_types" => staged.orbital_provision_types = map_from_legacy_json(js)?,
        "wallet_journal_timestamps" => staged.wallet_journal_timestamps = map_from_legacy_json(js)?,
        "char_wallet_transactions" => staged.char_wallet_transactions = map_from_legacy_json(js)?,
        "corp_wallet_journals" => staged.corp_wallet_journals = map_from_legacy_json(js)?,
        "corp_wallet_transactions" => staged.corp_wallet_transactions = map_from_legacy_json(js)?,
        "market_order_meta" => staged.market_order_meta = map_from_legacy_json(js)?,
        "market_history" => staged.market_history = map_from_legacy_json(js)?,
        "cosmic_signatures" => staged.cosmic_signatures = map_from_legacy_json(js)?,
        "cosmic_anomalies" => staged.cosmic_anomalies = map_from_legacy_json(js)?,
        "incursions" => staged.incursions = map_from_legacy_json(js)?,
        "last_mission_decline_times" => {
            staged.last_mission_decline_times = map_from_legacy_json(js)?
        }
        "module_online_states" => staged.module_online_states = map_from_legacy_json(js)?,
        "module_active_states" => staged.module_active_states = set_from_legacy_json(js)?,
        "module_active_charges" => staged.module_active_charges = map_from_legacy_json(js)?,
        "ship_fit_inputs" => staged.ship_fit_inputs = map_from_legacy_json(js)?,
        "weapon_banks" => staged.weapon_banks = nested_map_from_legacy_json(js)?,
        "paused_skill_queues" => staged.paused_skill_queues = set_from_legacy_json(js)?,
        "starbase_settings" => staged.starbase_settings = map_from_legacy_json(js)?,
        "killmail_details" => staged.killmail_details = map_from_legacy_json(js)?,
        "station_office_rental_costs" => {
            staged.station_office_rental_costs = map_from_legacy_json(js)?
        }
        "structure_esi_timer_meta" => staged.structure_esi_timer_meta = map_from_legacy_json(js)?,
        "corp_project_contributors" => {
            staged.corp_project_contributors = map_from_legacy_json(js)?
        }
        "loyalty_store_offers" => staged.loyalty_store_offers = map_from_legacy_json(js)?,
        "system_jump_events" => {
            staged.system_jump_events = serde_json::from_str(js).map_err(|e| e.to_string())?
        }
        "system_kill_events" => {
            staged.system_kill_events = serde_json::from_str(js).map_err(|e| e.to_string())?
        }
        "quasar_state_blobs" => staged.quasar_state_blobs = map_from_legacy_json(js)?,
        "service_state_blobs" => staged.service_state_blobs = map_from_legacy_json(js)?,
        "insurance_contracts" => staged.insurance_contracts = map_from_legacy_json(js)?,
        _ => return Ok(false),
    }
    Ok(true)
}

fn decode_legacy_world_kv_blobs(
    conn: &Connection,
    staged: &mut StagedIndustryMarketPve,
    ctx: &mut LoadContext,
) -> Result<bool, String> {
    if !table_exists(conn, "world_kv_blobs")? {
        return Ok(false);
    }
    let mut stmt = conn
        .prepare("SELECT rowid, blob_key, blob_json FROM world_kv_blobs ORDER BY rowid ASC")
        .map_err(|e| format!("legacy world_kv_blobs prepare: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| format!("legacy world_kv_blobs query: {e}"))?;
    for row in rows {
        let (rowid, key, js) = row.map_err(|e| format!("legacy world_kv_blobs row: {e}"))?;
        // Already owned/drained by Family 4 (`corp_crimewatch_store`).
        if matches!(key.as_str(), "corp_container_logs" | "wh_polarizations") {
            continue;
        }
        match decode_legacy_blob(staged, &key, &js) {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!("ignoring unknown legacy world_kv_blobs key '{key}'");
            }
            Err(err) => {
                ctx.record_bad_row(
                    "world_kv_blobs",
                    "world_kv_blobs",
                    &key,
                    &js,
                    err,
                    Some(rowid),
                )?;
            }
        }
    }
    Ok(true)
}

fn load_domain_rows_into_staged(
    conn: &Connection,
    staged: &mut StagedIndustryMarketPve,
    ctx: &mut LoadContext,
) -> Result<(), String> {
    macro_rules! load_all {
        (
            $(
                $dom_name:ident {
                    const_name: $domain_const:ident,
                    maps: [ $( $map_f:ident : $mk:ty => $mv:ty ),* $(,)? ],
                    sets: [ $( $set_f:ident : $sk:ty ),* $(,)? ],
                    scalars: [ $( $scalar_f:ident : $sv:ty ),* $(,)? ],
                }
            ),* $(,)?
        ) => {
            $(
                for (rowid, raw_key, raw_val) in fetch_domain_row_entries(conn, $domain_const)? {
                    let decode_res: Result<(), String> = (|| {
                        #[allow(clippy::match_single_binding)]
                        match raw_key.as_str() {
                            $(
                                stringify!($scalar_f) => {
                                    staged.$scalar_f = from_checked_domain_json(&raw_val)?;
                                    return Ok(());
                                }
                            )*
                            _ => {}
                        }
                        let (field, subkey) = raw_key.split_once('/').ok_or_else(|| {
                            format!("malformed domain_rows key '{raw_key}' for {}", $domain_const)
                        })?;
                        match field {
                            $(
                                stringify!($map_f) => {
                                    let (k, v) = decode_map_entry::<$mk, $mv>(subkey, &raw_val)?;
                                    staged.$map_f.insert(k, v);
                                    Ok(())
                                }
                            )*
                            $(
                                stringify!($set_f) => {
                                    let k = decode_set_entry::<$sk>(subkey, &raw_val)?;
                                    staged.$set_f.insert(k);
                                    Ok(())
                                }
                            )*
                            _ => Err(format!(
                                "unknown domain_rows key '{raw_key}' for {}",
                                $domain_const
                            )),
                        }
                    })();
                    if let Err(err) = decode_res {
                        ctx.record_bad_row(
                            "domain_rows",
                            $domain_const,
                            &raw_key,
                            &raw_val,
                            err,
                            Some(rowid),
                        )?;
                    }
                }
            )*
        };
    }

    for_each_industry_market_pve_domain!(load_all);

    for (rowid, raw_key, raw_val) in fetch_domain_row_entries(conn, DOMAIN_SERVICE_STATE_BLOBS)? {
        let decode_res: Result<(), String> = (|| {
            let (field, subkey) = raw_key.split_once('/').ok_or_else(|| {
                format!(
                    "malformed domain_rows key '{raw_key}' for {DOMAIN_SERVICE_STATE_BLOBS}"
                )
            })?;
            match field {
                "service_state_blobs" => {
                    let (k, v) = decode_map_entry::<String, String>(subkey, &raw_val)?;
                    staged.service_state_blobs.insert(k, v);
                    Ok(())
                }
                _ => Err(format!(
                    "unknown domain_rows key '{raw_key}' for {DOMAIN_SERVICE_STATE_BLOBS}"
                )),
            }
        })();
        if let Err(err) = decode_res {
            ctx.record_bad_row(
                "domain_rows",
                DOMAIN_SERVICE_STATE_BLOBS,
                &raw_key,
                &raw_val,
                err,
                Some(rowid),
            )?;
        }
    }

    Ok(())
}

fn load_staged_industry_market_pve(
    conn: &Connection,
    ctx: &mut LoadContext,
) -> Result<(StagedIndustryMarketPve, bool), String> {
    let mut staged = StagedIndustryMarketPve::default();
    let had_legacy = decode_legacy_world_kv_blobs(conn, &mut staged, ctx)?;
    load_domain_rows_into_staged(conn, &mut staged, ctx)?;
    Ok((staged, had_legacy))
}

/// Loads all 34 Family 5 (`industry_market_pve`) collections from `domain_rows` (overlaying any
/// unmigrated legacy `world_kv_blobs` entries so newer `domain_rows` wins) into `snap`.
///
/// Returns `Ok(true)` if the retired `world_kv_blobs` table existed and should be dropped once
/// migrated rows commit. Fails closed without mutating `snap` when `ctx.quarantine_unreadable` is
/// `false`.
pub fn load_industry_market_pve_domain_rows(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
    ctx: &mut LoadContext,
) -> Result<bool, String> {
    let (staged, had_legacy) = load_staged_industry_market_pve(conn, ctx)?;
    staged.apply_to(snap);
    Ok(had_legacy)
}

/// Convenience loader using an in-memory quarantine context (skips corrupt rows cleanly).
pub fn load_industry_market_pve(conn: &Connection, snap: &mut WorldStateSnapshot) {
    let _ =
        load_industry_market_pve_domain_rows(conn, snap, &mut LoadContext::quarantine_in_memory());
}

/// Drops the retired legacy `world_kv_blobs` table after migration to `domain_rows`.
pub(crate) fn drop_legacy_industry_market_pve_tables(tx: &Transaction<'_>) -> Result<(), String> {
    tx.execute("DROP TABLE IF EXISTS world_kv_blobs", [])
        .map_err(|e| format!("drop legacy world_kv_blobs table: {e}"))?;
    Ok(())
}

/// Collects per-entry `domain_rows` diffs across all 34 Family 5 collections without cloning
/// unrelated maps or triggering cold-domain full syncs.
pub(crate) fn collect_industry_market_pve_domain_row_diffs(
    before: &WorldStateSnapshot,
    after: &WorldStateSnapshot,
    out: &mut Vec<DomainRowChange>,
) {
    trait DomainMapDiff<K, V> {
        fn collect_map_diffs(
            &self,
            after: &Self,
            domain: &'static str,
            field: &'static str,
            out: &mut Vec<DomainRowChange>,
        );
    }

    impl<K: Eq + Hash + crate::sqlite_store::domain_rows::KeyCodec, V: PartialEq + serde::Serialize>
        DomainMapDiff<K, V> for HashMap<K, V>
    {
        #[inline]
        fn collect_map_diffs(
            &self,
            after: &Self,
            domain: &'static str,
            field: &'static str,
            out: &mut Vec<DomainRowChange>,
        ) {
            if !(self.is_empty() && after.is_empty()) {
                diff_map_with(
                    domain,
                    field,
                    self.len(),
                    after.iter(),
                    |k| self.get(k),
                    self.keys(),
                    |k| after.contains_key(k),
                    out,
                );
            }
        }
    }

    impl<K: Eq + Hash + crate::sqlite_store::domain_rows::KeyCodec, V: PartialEq + serde::Serialize>
        DomainMapDiff<K, V> for crate::cow::CowCell<HashMap<K, V>>
    {
        #[inline]
        fn collect_map_diffs(
            &self,
            after: &Self,
            domain: &'static str,
            field: &'static str,
            out: &mut Vec<DomainRowChange>,
        ) {
            if !self.ptr_eq(after) {
                self.as_inner()
                    .collect_map_diffs(after.as_inner(), domain, field, out);
            }
        }
    }

    impl<
            K: Ord + Clone + crate::sqlite_store::domain_rows::KeyCodec,
            V: Clone + PartialEq + serde::Serialize,
        > DomainMapDiff<K, V> for crate::cow::CowMap<K, V>
    {
        #[inline]
        fn collect_map_diffs(
            &self,
            after: &Self,
            domain: &'static str,
            field: &'static str,
            out: &mut Vec<DomainRowChange>,
        ) {
            crate::sqlite_store::domain_rows::diff_cow_map_field(
                domain, "", field, self, after, out,
            );
        }
    }

    trait MaybePtrEq {
        fn ptr_eq_fast(&self, other: &Self) -> bool;
    }

    impl<T> MaybePtrEq for crate::cow::CowCell<T> {
        #[inline]
        fn ptr_eq_fast(&self, other: &Self) -> bool {
            self.ptr_eq(other)
        }
    }

    impl<T> MaybePtrEq for HashSet<T> {
        #[inline]
        fn ptr_eq_fast(&self, _other: &Self) -> bool {
            false
        }
    }

    macro_rules! collect_all {
        (
            $(
                $dom_name:ident {
                    const_name: $domain_const:ident,
                    maps: [ $( $map_f:ident : $mk:ty => $mv:ty ),* $(,)? ],
                    sets: [ $( $set_f:ident : $sk:ty ),* $(,)? ],
                    scalars: [ $( $scalar_f:ident : $sv:ty ),* $(,)? ],
                }
            ),* $(,)?
        ) => {
            $(
                $(
                    before.$map_f.collect_map_diffs(
                        &after.$map_f,
                        $domain_const,
                        stringify!($map_f),
                        out,
                    );
                )*
                $(
                    if !before.$set_f.ptr_eq_fast(&after.$set_f) {
                        diff_set_with(
                            $domain_const,
                            stringify!($set_f),
                            before.$set_f.len(),
                            after.$set_f.iter(),
                            |k| before.$set_f.contains(k),
                            before.$set_f.iter(),
                            |k| after.$set_f.contains(k),
                            out,
                        );
                    }
                )*
                $(
                    if !before.$scalar_f.ptr_eq(&after.$scalar_f) {
                        let def: $sv = Default::default();
                        diff_scalar(
                            $domain_const,
                            stringify!($scalar_f),
                            &*before.$scalar_f,
                            &*after.$scalar_f,
                            &def,
                            out,
                        );
                    }
                )*
            )*
        };
    }

    for_each_industry_market_pve_domain!(collect_all);

    if !before.service_state_blobs.ptr_eq(&after.service_state_blobs)
        && before.service_state_blobs != after.service_state_blobs
    {
        if let Err(msg) = validate_service_state_blobs(&after.service_state_blobs) {
            record_service_state_blob_limit_violation(&msg);
        } else {
            diff_map_with(
                DOMAIN_SERVICE_STATE_BLOBS,
                "service_state_blobs",
                before.service_state_blobs.len(),
                after.service_state_blobs.iter(),
                |k| before.service_state_blobs.get(k),
                before.service_state_blobs.keys(),
                |k| after.service_state_blobs.contains_key(k),
                out,
            );
        }
    }
}

/// Synchronizes all 34 Family 5 collections into `domain_rows` cleanly within `tx`.
pub fn sync_industry_market_pve_domain_rows(
    tx: &Transaction<'_>,
    snap: &WorldStateSnapshot,
) -> Result<bool, String> {
    let mut changed = false;

    macro_rules! sync_all {
        (
            $(
                $dom_name:ident {
                    const_name: $domain_const:ident,
                    maps: [ $( $map_f:ident : $mk:ty => $mv:ty ),* $(,)? ],
                    sets: [ $( $set_f:ident : $sk:ty ),* $(,)? ],
                    scalars: [ $( $scalar_f:ident : $sv:ty ),* $(,)? ],
                }
            ),* $(,)?
        ) => {
            $(
                {
                    let mut state = DomainSyncState::load(tx, $domain_const)?;
                    $(
                        state.push_map(stringify!($map_f), snap.$map_f.iter())?;
                    )*
                    $(
                        state.push_set(stringify!($set_f), snap.$set_f.iter())?;
                    )*
                    $(
                        {
                            let def: $sv = Default::default();
                            state.push_scalar(stringify!($scalar_f), &*snap.$scalar_f, &def)?;
                        }
                    )*
                    changed |= state.finish(tx)?;
                }
            )*
        };
    }

    for_each_industry_market_pve_domain!(sync_all);

    if let Err(msg) = validate_service_state_blobs(&snap.service_state_blobs) {
        record_service_state_blob_limit_violation(&msg);
    } else {
        let mut state = DomainSyncState::load(tx, DOMAIN_SERVICE_STATE_BLOBS)?;
        state.push_map("service_state_blobs", snap.service_state_blobs.iter())?;
        changed |= state.finish(tx)?;
    }

    Ok(changed)
}

/// Synchronizes the remaining relational Family 5 table (`char_mining_ledger`) when dirty.
pub fn sync_mining_ledger(
    tx: &Transaction<'_>,
    before: Option<&WorldStateSnapshot>,
    snap: &WorldStateSnapshot,
) -> Result<bool, String> {
    if let Some(b) = before {
        if b.mining_ledger.ptr_eq(&snap.mining_ledger) || *b.mining_ledger == *snap.mining_ledger {
            return Ok(false);
        }
    }
    tx.execute("DELETE FROM char_mining_ledger", [])
        .map_err(|e| e.to_string())?;
    for entry in &snap.mining_ledger {
        tx.execute(
            "INSERT INTO char_mining_ledger (character_id, solar_system_id, type_id, quantity, date, timestamp_unix) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.character_id,
                entry.solar_system_id as i64,
                entry.type_id as i64,
                entry.quantity,
                entry.date,
                entry.timestamp_unix as i64,
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(true)
}
