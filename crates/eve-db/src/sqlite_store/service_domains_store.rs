//! SQLite persistence for `entity_kv` and Family 6 typed service domain registries (`#588`, `#916`).
//!
//! All 17 typed service domains decompose field- and map/set-entry-wise into `domain_rows`;
//! `entity_kv` remains in its dedicated `WITHOUT ROWID` table (`ENTITY_KV_SCHEMA_VERSION = 7`).

use super::domain_rows::{
    decode_map_entry, decode_set_entry, diff_map_with, diff_scalar, diff_set_with,
    fetch_domain_row_entries, from_checked_domain_json, table_exists, DomainRowChange,
    DomainSyncState, LegacyMerge, Persist, DOMAIN_ABYSSAL_STATE, DOMAIN_CLUSTER_AND_RUNTIME,
    DOMAIN_CRAB_BEACONS, DOMAIN_DEPLOYABLE_STATE, DOMAIN_DUNGEON_EDITOR, DOMAIN_DUNGEON_RUNTIME,
    DOMAIN_FLEET_AND_SOCIAL_OPS, DOMAIN_FOB_OBSERVATORY_STATE, DOMAIN_INSURGENCY_COMPLEX_OPS,
    DOMAIN_MINING_AND_ESS_STATE, DOMAIN_NODE_GRAPH_STATE, DOMAIN_PVE_SPECIAL_SITES,
    DOMAIN_PVP_FILAMENT_STATE, DOMAIN_QUASAR_COMMERCE, DOMAIN_SEASONS_AND_CAMPAIGNS,
    DOMAIN_SUPERWEAPON_RUNTIME, DOMAIN_TOURNEY_STATE,
};
use super::fingerprints::{canonical_fp, TableFingerprints};
use crate::service_domains::{
    AbyssalRuntimeStore, ClusterAndRuntimeStore, CrabBeaconStore, DungeonEditorStore,
    DungeonRuntimeStore, EntityKvKey, EntityKvRow, FleetAndSocialOpsStore, FobObservatoryStore,
    InsurgencyComplexOpsState, MiningAndEssRuntimeStore, MobileDeployableRuntimeState,
    NodeGraphAndProgressionStore, PersistedSuperweaponRuntime, PveSpecialSitesState,
    PvpFilamentStore, QuasarCommerceStore, SeasonsAndCampaignsStore, TourneyStore,
};
use crate::types::WorldStateSnapshot;
use rusqlite::{params, Connection, Transaction};
use serde::Serialize;
use std::collections::BTreeMap;

pub const ENTITY_KV_SCHEMA_VERSION: i64 = 7;

#[cfg(test)]
pub(crate) const LEGACY_SERVICE_DOMAIN_STATE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS service_domain_state (
    domain_key TEXT PRIMARY KEY,
    payload_json TEXT NOT NULL
);
"#;

const ENTITY_KV_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS entity_kv (
    ns           TEXT    NOT NULL,
    entity_id    INTEGER NOT NULL,
    sub_key      TEXT    NOT NULL DEFAULT '',
    value        BLOB    NOT NULL,
    updated_unix INTEGER NOT NULL,
    PRIMARY KEY (ns, entity_id, sub_key)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS entity_kv_by_entity ON entity_kv(entity_id, ns);
"#;

/// Fingerprints for `entity_kv` (all 17 typed service domains migrated to `domain_rows` in `#916`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceDomainsFingerprints {
    pub entity_kv_fp: u64,
    pub entity_kv_rows: BTreeMap<EntityKvKey, u64>,
}

/// Applies the schema DDL for `entity_kv` (legacy `service_domain_state` now lives in `domain_rows`).
pub fn apply_service_domains_migration(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(ENTITY_KV_SCHEMA_SQL)
        .map_err(|e| e.to_string())?;
    let now_unix = crate::clock::host_wall_unix_secs_and_nanos().0 as i64;
    conn.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, name, applied_unix) VALUES (?1, ?2, ?3)",
        params![ENTITY_KV_SCHEMA_VERSION, "0007_entity_kv", now_unix],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn fp_domain<T: Default + PartialEq + Serialize>(val: &T) -> u64 {
    if *val == T::default() {
        return 0;
    }
    let out = canonical_fp(val);
    if out == 0 {
        1
    } else {
        out
    }
}

pub fn compute_service_domains_fingerprints(
    snap: &WorldStateSnapshot,
) -> ServiceDomainsFingerprints {
    let mut fp = ServiceDomainsFingerprints {
        entity_kv_fp: fp_domain(&snap.entity_kv),
        entity_kv_rows: BTreeMap::new(),
    };
    for (k, v) in &snap.entity_kv {
        let rfp = canonical_fp(v);
        fp.entity_kv_rows.insert(k.clone(), if rfp == 0 { 1 } else { rfp });
    }
    fp
}

macro_rules! impl_service_domain_persist {
    (
        $ty:ty,
        maps: [ $($map_f:ident),* $(,)? ],
        sets: [ $($set_f:ident),* $(,)? ],
        scalars: [ $($scalar_f:ident),* $(,)? ]
    ) => {
        impl Persist for $ty {
            fn collect_diff_ops(
                &self,
                after: &Self,
                domain: &'static str,
                out: &mut Vec<DomainRowChange>,
            ) {
                let def = Self::default();
                let _ = &def;
                $(
                    if !(self.$map_f.is_empty() && after.$map_f.is_empty()) {
                        diff_map_with(
                            domain,
                            stringify!($map_f),
                            self.$map_f.len(),
                            after.$map_f.iter(),
                            |k| self.$map_f.get(k),
                            self.$map_f.keys(),
                            |k| after.$map_f.contains_key(k),
                            out,
                        );
                    }
                )*
                $(
                    if !(self.$set_f.is_empty() && after.$set_f.is_empty()) {
                        diff_set_with(
                            domain,
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
                    diff_scalar(
                        domain,
                        stringify!($scalar_f),
                        &self.$scalar_f,
                        &after.$scalar_f,
                        &def.$scalar_f,
                        out,
                    );
                )*
            }

            fn sync_full(&self, tx: &Transaction<'_>, domain: &'static str) -> Result<bool, String> {
                let mut state = DomainSyncState::load(tx, domain)?;
                let def = Self::default();
                let _ = &def;
                $(
                    state.push_map(stringify!($map_f), self.$map_f.iter())?;
                )*
                $(
                    state.push_set(stringify!($set_f), self.$set_f.iter())?;
                )*
                $(
                    state.push_scalar(stringify!($scalar_f), &self.$scalar_f, &def.$scalar_f)?;
                )*
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
                    let split = match raw_key.split_once('/') {
                        Some((f, sub)) => (f, Some(sub)),
                        None => (raw_key.as_str(), None),
                    };
                    let res: Result<(), String> = match split {
                        $(
                            (stringify!($map_f), Some(sub)) => {
                                decode_map_entry(sub, &raw_val).map(|(k, v)| {
                                    out.$map_f.insert(k, v);
                                })
                            }
                        )*
                        $(
                            (stringify!($set_f), Some(sub)) => {
                                decode_set_entry(sub, &raw_val).map(|k| {
                                    out.$set_f.insert(k);
                                })
                            }
                        )*
                        $(
                            (stringify!($scalar_f), None) => {
                                from_checked_domain_json(&raw_val).map(|v| {
                                    out.$scalar_f = v;
                                })
                            }
                        )*
                        _ => Err(format!("unknown domain_rows key '{raw_key}' for {domain}")),
                    };
                    if let Err(err) = res {
                        ctx.record_bad_row("domain_rows", domain, raw_key, raw_val, err, Some(rowid))?;
                    }
                }
                Ok(out)
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
                    for k in legacy.$set_f {
                        self.$set_f.insert(k);
                    }
                )*
                $(
                    if self.$scalar_f == def.$scalar_f && legacy.$scalar_f != def.$scalar_f {
                        self.$scalar_f = legacy.$scalar_f;
                    }
                )*
            }
        }
    };
}

impl_service_domain_persist!(
    PveSpecialSitesState,
    maps: [
        incursions,
        incursion_lifecycle,
        incursion_site_waves,
        world_event_tales,
        incursion_lp_logs,
        homefront_sites,
        drifter_hives,
        triglavian_sites,
        interference_by_system,
        seasonal_event_sites,
    ],
    sets: [],
    scalars: [
        initialized,
        roaming_storms,
        edencom_fortress_systems,
        edencom_minor_victory_systems,
        triglavian_minor_victory_systems,
        sim_clock_sec,
        next_seq,
    ]
);

impl_service_domain_persist!(
    CrabBeaconStore,
    maps: [beacons],
    sets: [],
    scalars: []
);

impl_service_domain_persist!(
    AbyssalRuntimeStore,
    maps: [
        pockets,
        traces,
        void_space_sessions,
        active_clouds,
        boundary_ticks,
        void_space_content,
    ],
    sets: [deathzone_active_ships],
    scalars: [next_seq]
);

impl_service_domain_persist!(
    DungeonRuntimeStore,
    maps: [instances],
    sets: [],
    scalars: [next_instance_id]
);

impl_service_domain_persist!(
    DungeonEditorStore,
    maps: [rooms_by_dungeon, dungeon_disallow_mwd, active_editors],
    sets: [],
    scalars: [next_custom_room_id, next_custom_object_id]
);

impl_service_domain_persist!(
    FobObservatoryStore,
    maps: [
        observatory_sites,
        observatory_respawn_until,
        fob_combatants,
        fob_factions,
        fob_respawn_until,
    ],
    sets: [fob_defense_triggered],
    scalars: []
);

impl_service_domain_persist!(
    MiningAndEssRuntimeStore,
    maps: [
        ess_reserve_pulses,
        phased_asteroid_fields,
        mining_ledger_waste,
    ],
    sets: [],
    scalars: [
        phased_field_last_tick_unix,
        moon_fractured_fields,
        metenox_last_tick_unix,
    ]
);

impl_service_domain_persist!(
    PvpFilamentStore,
    maps: [
        queue,
        stats,
        event_stats,
        active_matches,
        match_teams,
        match_eliminated,
        match_arena_offsets,
        match_filament_types,
        hazard_ticks,
        queue_refund_targets,
        queue_tickets,
        origin_positions,
    ],
    sets: [],
    scalars: [events, seeded_default_events, next_ticket_id, next_match_id]
);

impl_service_domain_persist!(
    TourneyStore,
    maps: [matches, ban_sessions, tournaments, arena_systems],
    sets: [],
    scalars: [active_match_id]
);

impl_service_domain_persist!(
    MobileDeployableRuntimeState,
    maps: [
        deployables,
        damaged_items,
        observatory_pings_done,
        cloak_activated_unix,
        mmju_spools,
        siphon_last_cycle_unix,
    ],
    sets: [],
    scalars: [pending_decloak_notifications]
);

impl_service_domain_persist!(
    PersistedSuperweaponRuntime,
    maps: [],
    sets: [],
    scalars: [active_cycles, warp_suppressions]
);

impl_service_domain_persist!(
    InsurgencyComplexOpsState,
    maps: [
        outpost_raids,
        mining_ambushes,
        subpoint_remainders,
        named_roamer_rolls,
        ice_heist_respawn_until,
    ],
    sets: [],
    scalars: []
);

impl_service_domain_persist!(
    NodeGraphAndProgressionStore,
    maps: [
        air_npe_runners,
        client_node_graphs,
        node_graph_blackboards,
        expert_systems,
        certificates,
    ],
    sets: [],
    scalars: [client_node_graph_seq]
);

impl_service_domain_persist!(
    SeasonsAndCampaignsStore,
    maps: [
        season_states,
        active_seasons,
        seasons_catalog,
        season_loot_tables,
        announced_season_states,
        hacking_wins,
        milestone_playtime,
        login_campaign_enrollments,
        auto_injected_rewards,
        login_reward_expiry,
        project_discovery_players,
        project_discovery_votes,
        character_activities,
        fast_checkout_states,
    ],
    sets: [],
    scalars: [
        season_configuration,
        challenge_definitions,
        announced_season_init,
        daily_login_campaign_schedule,
        seasonal_login_schedule,
        project_discovery_store,
    ]
);

impl_service_domain_persist!(
    QuasarCommerceStore,
    maps: [nes_store_offer_tags, targeted_offers],
    sets: [lp_asset_ops_completed],
    scalars: [
        payment_tokens,
        payment_receipts,
        payment_schedules,
        plex_tx_logs,
        nes_store_catalog,
        plex_invoices,
        cosmetics_market,
        orchestrator,
        failed_fulfillments,
        acl_registry,
    ]
);

impl_service_domain_persist!(
    FleetAndSocialOpsStore,
    maps: [
        fleet_saved_setups,
        fleet_stored_formations,
        fleet_adverts,
        fleet_join_requests,
        fleet_watchlist,
        fleet_voice_enabled,
        fleet_loot_logging,
        corp_allow_war,
        hypernet_subs,
        contract_deposits,
        delayed_rewards,
        agent_char_disabled_missions,
        agent_replay_ts,
        signal_tracker_subs,
        wh_local_witnesses,
    ],
    sets: [mission_taxed_journal, agent_global_disabled_missions],
    scalars: [
        corp_recruitment_ads,
        ingamereport_tickets,
        lp_store_offers,
        pending_probe_scans,
    ]
);

impl_service_domain_persist!(
    ClusterAndRuntimeStore,
    maps: [
        structure_defense,
        alliance_sov_cfg,
        session_registry,
        cluster_service_states,
        macho_global_config,
        alert_bean_counts,
        alert_log_modes,
        localization_qa_wrap_users,
        portraits,
        paperdoll_states,
        char_system_visits,
        module_reload_until_unix,
        auto_reloaded_event,
        warp_vector_cooldown_until,
        ship_clone_offers,
        mct_slots,
        account_resculpt_tokens,
        char_cohorts,
        user_cohorts,
        character_locks,
    ],
    sets: [resculpt_pending, omega_downgraded],
    scalars: [
        cluster_misc_state,
        clone_vat_bay_state,
        load_service_state,
        active_sessions,
        cluster_node_table,
        cluster_admin_broadcasts,
        cluster_attacks,
        external_queue_events,
        cluster_downtime_state,
        macho_client_code_hash,
        user_sessions,
        gm_audit_log,
        alert_client_traces,
        alert_proxy_traces,
        alert_group_bean_bytes,
        browser_lockdown_policy,
        client_stat_logger_strings,
        blacklisted_planets,
        server_runtime_json,
    ]
);

// ============================================================================
// Typed Service Domain Table Dispatch & Legacy Migration
// ============================================================================

macro_rules! for_each_service_domain_spec {
    ($cb:ident $($extra:tt)*) => {
        $cb!(
            $($extra)*
            (pve_special_sites, PveSpecialSitesState, DOMAIN_PVE_SPECIAL_SITES, "pve_special_sites"),
            (crab_beacons, CrabBeaconStore, DOMAIN_CRAB_BEACONS, "crab_beacons"),
            (abyssal_state, AbyssalRuntimeStore, DOMAIN_ABYSSAL_STATE, "abyssal_state"),
            (dungeon_runtime, DungeonRuntimeStore, DOMAIN_DUNGEON_RUNTIME, "dungeon_runtime"),
            (dungeon_editor, DungeonEditorStore, DOMAIN_DUNGEON_EDITOR, "dungeon_editor"),
            (fob_observatory_state, FobObservatoryStore, DOMAIN_FOB_OBSERVATORY_STATE, "fob_observatory_state"),
            (mining_and_ess_state, MiningAndEssRuntimeStore, DOMAIN_MINING_AND_ESS_STATE, "mining_and_ess_state"),
            (pvp_filament_state, PvpFilamentStore, DOMAIN_PVP_FILAMENT_STATE, "pvp_filament_state"),
            (tourney_state, TourneyStore, DOMAIN_TOURNEY_STATE, "tourney_state"),
            (deployable_state, MobileDeployableRuntimeState, DOMAIN_DEPLOYABLE_STATE, "deployable_state"),
            (superweapon_runtime, PersistedSuperweaponRuntime, DOMAIN_SUPERWEAPON_RUNTIME, "superweapon_runtime"),
            (insurgency_complex_ops, InsurgencyComplexOpsState, DOMAIN_INSURGENCY_COMPLEX_OPS, "insurgency_complex_ops"),
            (node_graph_state, NodeGraphAndProgressionStore, DOMAIN_NODE_GRAPH_STATE, "node_graph_state"),
            (seasons_and_campaigns, SeasonsAndCampaignsStore, DOMAIN_SEASONS_AND_CAMPAIGNS, "seasons_and_campaigns"),
            (quasar_commerce, QuasarCommerceStore, DOMAIN_QUASAR_COMMERCE, "quasar_commerce"),
            (fleet_and_social_ops, FleetAndSocialOpsStore, DOMAIN_FLEET_AND_SOCIAL_OPS, "fleet_and_social_ops"),
            (cluster_and_runtime, ClusterAndRuntimeStore, DOMAIN_CLUSTER_AND_RUNTIME, "cluster_and_runtime"),
        );
    };
}

macro_rules! define_staged_service_domains {
    ($(($field:ident, $ty:ty, $domain:ident, $legacy:expr),)+) => {
        struct StagedServiceDomains {
            $($field: $ty,)+
        }

        impl StagedServiceDomains {
            fn load_from_domain_rows(
                conn: &Connection,
                ctx: &mut super::quarantine::LoadContext,
            ) -> Result<Self, String> {
                Ok(Self {
                    $($field: <$ty>::load_domain(conn, $domain, ctx)?,)+
                })
            }

            fn merge_legacy_row(&mut self, domain_key: &str, raw_payload: &str) -> Option<Result<(), String>> {
                match domain_key {
                    $(
                        $legacy => Some(
                            from_checked_domain_json::<$ty>(raw_payload).map(|val| {
                                self.$field.merge_legacy(val);
                            }),
                        ),
                    )+
                    _ => None,
                }
            }

            fn apply_to(self, snap: &mut WorldStateSnapshot) {
                $(snap.$field = self.$field;)+
            }
        }
    };
}

for_each_service_domain_spec!(define_staged_service_domains);

fn validate_legacy_blob_table(
    conn: &Connection,
    sql: &str,
    source_table: &'static str,
    table_display: &str,
    ctx: &mut super::quarantine::LoadContext,
) -> Result<(), String> {
    let Ok(mut stmt) = conn.prepare(sql) else {
        return Ok(());
    };
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(());
    };
    while let Some(r) = rows.next().map_err(|e| e.to_string())? {
        let rowid = r.get::<_, i64>(0).ok();
        let key = super::quarantine::row_col_string(r, 1);
        let raw_payload = super::quarantine::row_col_string(r, 2);
        let parsed = r
            .get::<_, String>(2)
            .map_err(|e| e.to_string())
            .and_then(|s| from_checked_domain_json::<serde_json::Value>(&s));
        if let Err(err) = parsed {
            ctx.record_bad_row(source_table, table_display, key, raw_payload, err, rowid)?;
        }
    }
    Ok(())
}

fn stage_service_domains_rows(
    conn: &Connection,
    ctx: &mut super::quarantine::LoadContext,
) -> Result<(StagedServiceDomains, bool), String> {
    let mut staged = StagedServiceDomains::load_from_domain_rows(conn, ctx)?;
    let had_legacy = table_exists(conn, "service_domain_state")?;

    if had_legacy {
        let mut stmt = conn
            .prepare("SELECT rowid, domain_key, payload_json FROM service_domain_state")
            .map_err(|e| format!("legacy service_domain_state prepare: {e}"))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| format!("legacy service_domain_state query: {e}"))?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = super::quarantine::row_col_string(r, 1);
            let raw_payload = super::quarantine::row_col_string(r, 2);
            let parse_res = r
                .get::<_, String>(2)
                .map_err(|e| e.to_string())
                .and_then(|s| match staged.merge_legacy_row(&key, &s) {
                    Some(res) => res,
                    None => from_checked_domain_json::<serde_json::Value>(&s).map(|_| ()),
                });
            if let Err(err) = parse_res {
                ctx.record_bad_row(
                    "service_domain_state",
                    "service_domain_state (service_domain_blobs)",
                    key,
                    raw_payload,
                    err,
                    rowid,
                )?;
            }
        }
    }

    validate_legacy_blob_table(
        conn,
        "SELECT rowid, domain_key, COALESCE(payload_json, domain_json) FROM service_domain_blobs",
        "service_domain_blobs",
        "service_domain_blobs (service_domain_state)",
        ctx,
    )?;

    Ok((staged, had_legacy))
}

/// Drops the retired legacy `service_domain_state` table once migrated rows are flushed into `domain_rows`.
pub(crate) fn drop_legacy_service_domain_tables(tx: &Transaction<'_>) -> Result<(), String> {
    tx.execute_batch("DROP TABLE IF EXISTS service_domain_state;")
        .map_err(|e| format!("drop legacy service_domain_state table: {e}"))
}

/// Synchronizes all 17 Family 6 service domains into `domain_rows`.
pub(crate) fn sync_service_domains_rows(
    tx: &Transaction<'_>,
    snap: &WorldStateSnapshot,
) -> Result<bool, String> {
    let mut dirty = false;
    macro_rules! sync_domains {
        ($(($field:ident, $ty:ty, $domain:ident, $legacy:expr),)+) => {
            $(dirty |= snap.$field.sync_full(tx, $domain)?;)+
        };
    }
    for_each_service_domain_spec!(sync_domains);
    Ok(dirty)
}

// ============================================================================
// `entity_kv` Persistence (unchanged in `#916`)
// ============================================================================

fn read_entity_kv_map(
    conn: &Connection,
    ctx: &mut super::quarantine::LoadContext,
    fail_on_prepare_err: bool,
) -> Result<BTreeMap<EntityKvKey, EntityKvRow>, String> {
    let mut out = BTreeMap::new();
    let mut stmt = match conn
        .prepare("SELECT ns, entity_id, sub_key, value, updated_unix FROM entity_kv")
    {
        Ok(s) => s,
        Err(e) => {
            if fail_on_prepare_err {
                return Err(format!("rollback entity_kv prepare failed: {e}"));
            }
            return Ok(out);
        }
    };
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    let mut quarantined_pks: Vec<(rusqlite::types::Value, rusqlite::types::Value, rusqlite::types::Value)> =
        Vec::new();
    while let Some(r) = rows.next().map_err(|e| e.to_string())? {
        let key_display = format!(
            "{}/{}/{}",
            super::quarantine::row_col_string(r, 0),
            super::quarantine::row_col_string(r, 1),
            super::quarantine::row_col_string(r, 2)
        );
        let raw_payload = super::quarantine::row_col_string(r, 3);
        let parsed_res = (|| -> Result<(EntityKvKey, EntityKvRow), String> {
            let ns_str: String = r.get(0).map_err(|e| e.to_string())?;
            let entity_id: i64 = r.get(1).map_err(|e| e.to_string())?;
            let sub_key: String = r.get(2).map_err(|e| e.to_string())?;
            let value_json = match r.get_ref(3).map_err(|e| e.to_string())? {
                rusqlite::types::ValueRef::Blob(b) | rusqlite::types::ValueRef::Text(b) => {
                    std::str::from_utf8(b)
                        .map(|s| s.to_owned())
                        .map_err(|e| format!("entity_kv value is not valid UTF-8: {e}"))?
                }
                other => {
                    return Err(format!(
                        "entity_kv value has unexpected SQLite column type {:?}",
                        other.data_type()
                    ));
                }
            };
            let updated_unix_i64: i64 = r.get(4).map_err(|e| e.to_string())?;
            let ns = crate::service_domains::Ns::from_str_name(&ns_str)
                .ok_or_else(|| format!("unknown entity_kv namespace '{ns_str}'"))?;
            crate::service_domains::validate_entity_kv_value(ns, &value_json)?;
            Ok((
                EntityKvKey {
                    ns,
                    entity_id,
                    sub_key,
                },
                EntityKvRow {
                    value_json,
                    updated_unix: updated_unix_i64.max(0) as u64,
                },
            ))
        })();
        match parsed_res {
            Ok((k, v)) => {
                out.insert(k, v);
            }
            Err(err) => {
                ctx.record_bad_row("entity_kv", "entity_kv", key_display, raw_payload, err, None)?;
                if let (Ok(c0), Ok(c1), Ok(c2)) = (
                    r.get::<_, rusqlite::types::Value>(0),
                    r.get::<_, rusqlite::types::Value>(1),
                    r.get::<_, rusqlite::types::Value>(2),
                ) {
                    quarantined_pks.push((c0, c1, c2));
                }
            }
        }
    }
    drop(rows);
    drop(stmt);
    if !quarantined_pks.is_empty()
        && ctx.quarantine_unreadable
        && ctx.persist_quarantine
        && conn.is_autocommit()
    {
        for (c0, c1, c2) in quarantined_pks {
            let _ = conn.execute(
                "DELETE FROM entity_kv WHERE ns IS ?1 AND entity_id IS ?2 AND sub_key IS ?3",
                params![c0, c1, c2],
            );
        }
    }
    Ok(out)
}

pub fn sync_entity_kv(
    tx: &Transaction,
    snap: &WorldStateSnapshot,
    fp: &mut TableFingerprints,
) -> Result<bool, String> {
    let cur_fp = fp_domain(&snap.entity_kv);
    if cur_fp == fp.service_domains.entity_kv_fp
        && snap.entity_kv.len() == fp.service_domains.entity_kv_rows.len()
    {
        return Ok(false);
    }
    let mut changed = false;
    let mut new_rows = BTreeMap::new();
    for (k, row) in &snap.entity_kv {
        let rfp = {
            let h = canonical_fp(row);
            if h == 0 {
                1
            } else {
                h
            }
        };
        if fp.service_domains.entity_kv_rows.get(k).copied() != Some(rfp) {
            crate::service_domains::validate_entity_kv_value(k.ns, &row.value_json)?;
            tx.execute(
                "INSERT OR REPLACE INTO entity_kv (ns, entity_id, sub_key, value, updated_unix) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    k.ns.as_str(),
                    k.entity_id,
                    k.sub_key,
                    row.value_json.as_bytes(),
                    row.updated_unix as i64
                ],
            )
            .map_err(|e| e.to_string())?;
            changed = true;
        }
        new_rows.insert(k.clone(), rfp);
    }
    for old_k in fp.service_domains.entity_kv_rows.keys() {
        if !snap.entity_kv.contains_key(old_k) {
            tx.execute(
                "DELETE FROM entity_kv WHERE ns = ?1 AND entity_id = ?2 AND sub_key = ?3",
                params![old_k.ns.as_str(), old_k.entity_id, old_k.sub_key],
            )
            .map_err(|e| e.to_string())?;
            changed = true;
        }
    }
    fp.service_domains.entity_kv_fp = cur_fp;
    fp.service_domains.entity_kv_rows = new_rows;
    Ok(changed)
}

fn load_entity_kv_for_rollback(
    conn: &Connection,
    fp: &TableFingerprints,
) -> Result<BTreeMap<EntityKvKey, EntityKvRow>, String> {
    let mut ctx = super::quarantine::LoadContext::new(false);
    let out = read_entity_kv_map(conn, &mut ctx, true)?;
    if out.is_empty() && !fp.service_domains.entity_kv_rows.is_empty() {
        return Err("rollback entity_kv returned empty table for non-empty baseline".into());
    }
    for (k, &expected_row_fp) in &fp.service_domains.entity_kv_rows {
        match out.get(k) {
            Some(row)
                if {
                    let h = canonical_fp(row);
                    (if h == 0 { 1 } else { h }) == expected_row_fp
                } => {}
            Some(_) => {
                return Err(format!(
                    "rollback entity_kv mismatched baseline row ({}:{}:{})",
                    k.ns.as_str(),
                    k.entity_id,
                    k.sub_key
                ));
            }
            None => {
                return Err(format!(
                    "rollback entity_kv missing baseline row ({}:{}:{})",
                    k.ns.as_str(),
                    k.entity_id,
                    k.sub_key
                ));
            }
        }
    }
    Ok(out)
}

/// Loads all 17 typed service domains (from `domain_rows` and optional legacy `service_domain_state`)
/// plus `entity_kv`, committing to `snap` only after all staged queries and parses succeed.
///
/// Returns `true` when legacy `service_domain_state` existed and awaits dropping once committed.
pub fn load_service_domains_checked(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
    ctx: &mut super::quarantine::LoadContext,
) -> Result<bool, String> {
    let (staged, had_legacy) = stage_service_domains_rows(conn, ctx)?;
    let staged_entity_kv = read_entity_kv_map(conn, ctx, false)?;
    staged.apply_to(snap);
    snap.entity_kv = staged_entity_kv;
    Ok(had_legacy)
}

/// Convenience loader using an in-memory quarantine context (skips corrupt rows cleanly).
pub fn load_service_domains(conn: &Connection, snap: &mut WorldStateSnapshot) {
    let _ = load_service_domains_checked(
        conn,
        snap,
        &mut super::quarantine::LoadContext::quarantine_in_memory(),
    );
}

/// Synchronizes `entity_kv` changes (service domain rows sync incrementally via `domain_rows`).
pub fn sync_service_domains(
    tx: &Transaction,
    snap: &WorldStateSnapshot,
    fp: &mut TableFingerprints,
) -> Result<bool, String> {
    sync_entity_kv(tx, snap, fp)
}

fn rollback_service_domains_inner(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
    fp: &TableFingerprints,
) -> Result<(), String> {
    let has_domain_rows = table_exists(conn, "domain_rows")?;
    let has_legacy_table = table_exists(conn, "service_domain_state")?;
    if !has_domain_rows && !has_legacy_table {
        return Err("rollback service_domains missing domain_rows table".into());
    }
    let staged_entity_kv = if fp_domain(&snap.entity_kv) != fp.service_domains.entity_kv_fp
        || snap.entity_kv.len() != fp.service_domains.entity_kv_rows.len()
    {
        Some(load_entity_kv_for_rollback(conn, fp)?)
    } else {
        None
    };
    let mut ctx = super::quarantine::LoadContext::new(false);
    let (staged, _) = stage_service_domains_rows(conn, &mut ctx)?;
    staged.apply_to(snap);
    if let Some(kv) = staged_entity_kv {
        snap.entity_kv = kv;
    }
    Ok(())
}

/// Restores dirty typed service domains (from `domain_rows`) and `entity_kv` from SQLite into
/// temporaries, mutating `snap` only after every required query and parse succeeds.
pub fn rollback_service_domains(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
    fp: &TableFingerprints,
) -> Result<(), String> {
    if let Err(err) = rollback_service_domains_inner(conn, snap, fp) {
        crate::persistence::record_persist_failure();
        tracing::error!("rollback_service_domains failed closed without mutating snapshot: {err}");
        return Err(err);
    }
    Ok(())
}
