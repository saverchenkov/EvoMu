//! Serializable domain state types for `D-6` Batch 3: Corporation Governance, Crimewatch, and Warfare durability.

use crate::types::IskCents;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

pub mod tuple_map_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;
    use std::hash::Hash;

    pub fn serialize<S, K1, K2, V>(
        map: &HashMap<(K1, K2), V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        K1: Serialize + Copy + Ord,
        K2: Serialize + Copy + Ord,
        V: Serialize,
    {
        let mut entries: Vec<((K1, K2), &V)> =
            map.iter().map(|(&(k1, k2), v)| ((k1, k2), v)).collect();
        entries.sort_by_key(|&(k, _)| k);
        entries.serialize(serializer)
    }

    pub fn deserialize<'de, D, K1, K2, V>(deserializer: D) -> Result<HashMap<(K1, K2), V>, D::Error>
    where
        D: Deserializer<'de>,
        K1: Deserialize<'de> + Eq + Hash,
        K2: Deserialize<'de> + Eq + Hash,
        V: Deserialize<'de>,
    {
        let entries: Vec<((K1, K2), V)> = Vec::deserialize(deserializer)?;
        Ok(entries.into_iter().collect())
    }
}

pub mod sorted_set_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashSet;
    use std::hash::Hash;

    pub fn serialize<S, T>(set: &HashSet<T>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: Serialize + Copy + Ord,
    {
        let mut items: Vec<T> = set.iter().copied().collect();
        items.sort();
        items.serialize(serializer)
    }

    pub fn deserialize<'de, D, T>(deserializer: D) -> Result<HashSet<T>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de> + Eq + Hash,
    {
        let items: Vec<T> = Vec::deserialize(deserializer)?;
        Ok(items.into_iter().collect())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CorpGovState {
    #[serde(default)]
    pub role_audit_log: Vec<(i64, i64, u64, u64, u64)>,
    #[serde(default)]
    pub pending_role_grants: HashMap<i64, (u64, u64)>,
    #[serde(default)]
    pub pending_scoped_role_grants: HashMap<i64, (u64, u64, u64, u64)>,
    #[serde(default)]
    pub pending_auto_kicks: HashMap<i64, u64>,
    #[serde(default)]
    pub member_account_keys: HashMap<i64, i64>,
    #[serde(default)]
    pub member_notes: HashMap<i64, String>,
    #[serde(default)]
    pub action_log: Vec<(String, i64, u64)>,
    #[serde(default)]
    pub titles: HashMap<i64, (String, u64)>,
    #[serde(default)]
    pub member_titles: HashMap<i64, u64>,
    #[serde(default)]
    pub shares: HashMap<i64, i64>,
    #[serde(default)]
    pub company_shares: i64,
    #[serde(default)]
    pub bulletins: Vec<(i64, String, String, i64, u64)>,
    #[serde(default)]
    pub recruitment_ads: Vec<(i64, String, String, u64, i64)>,
    #[serde(default)]
    pub contacts: HashMap<i64, (f64, u64)>,
    #[serde(default)]
    pub labels: HashMap<i64, (String, u32)>,
    #[serde(default)]
    pub welcome_mail: (bool, String, String),
    #[serde(default)]
    pub restrict_corp_mails: bool,
    #[serde(default)]
    pub accept_structures: bool,
    #[serde(default)]
    pub accept_structures_set: bool,
    #[serde(default)]
    pub reinforce_hour: u8,
    #[serde(default)]
    pub friendly_fire_legal: bool,
    #[serde(default)]
    pub pending_friendly_fire: Option<(bool, u64)>,
    #[serde(default)]
    pub member_limit: Option<usize>,
    #[serde(default)]
    pub offices: HashMap<i64, i64>,
    #[serde(default)]
    pub logo: (i64, i64, i64, i64),
    #[serde(default)]
    pub logo_extra_colors: (i64, i64),
    #[serde(default)]
    pub applications_enabled: Option<bool>,
    #[serde(default)]
    pub medals: Vec<(i64, String, String, i64, u64)>,
    #[serde(default)]
    pub medal_recipients: Vec<(i64, i64, i64, String, i64)>,
    #[serde(default)]
    pub medal_graphics: HashMap<i64, Vec<(i64, i64, String, i64)>>,
    #[serde(default)]
    pub member_bases: HashMap<i64, i64>,
    #[serde(default)]
    pub member_divisions: HashMap<i64, (i64, i64)>,
    #[serde(default)]
    pub next_bulletin_id: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersistedAllianceMeta {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub logo: (i64, i64, i64, i64),
    #[serde(default)]
    pub emblem_entitlements: bool,
    #[serde(default)]
    pub executor_votes: HashMap<i64, i64>,
    #[serde(default)]
    pub contacts: HashMap<i64, (f64, u64)>,
    #[serde(default)]
    pub bulletins: Vec<(i64, String, String, i64, u64)>,
    #[serde(default)]
    pub applications: Vec<(i64, String, u64, String, Option<u64>)>,
    #[serde(default)]
    pub pending_leaves: HashMap<i64, u64>,
    #[serde(default)]
    pub employment_history: Vec<(i64, u64, Option<u64>)>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersistedAllianceWarState {
    #[serde(default)]
    pub alliances: HashMap<i64, PersistedAllianceMeta>,
    #[serde(default)]
    pub project_meta: HashMap<i64, (String, bool, bool)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedWarMeta {
    #[serde(default)]
    pub war_hq_id: Option<i64>,
    #[serde(default)]
    pub open_for_allies: bool,
    #[serde(default)]
    pub finish_at_unix: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PersistedNegKind {
    Surrender,
    Ally,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedWarNegotiation {
    pub id: i64,
    pub war_id: i64,
    pub kind: PersistedNegKind,
    pub from_corp: i64,
    pub isk: IskCents,
    pub message: String,
    pub status: String,
    pub effective_unix: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedPeaceTreaty {
    pub war_id: i64,
    pub corp_a: i64,
    pub corp_b: i64,
    pub expiry_unix: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedWarState {
    #[serde(default)]
    pub wars: HashMap<i64, PersistedWarMeta>,
    #[serde(default)]
    pub negotiations: Vec<PersistedWarNegotiation>,
    #[serde(default)]
    pub next_negotiation_id: i64,
    #[serde(default, with = "tuple_map_serde")]
    pub mutual_invites: HashMap<(i64, i64), u64>,
    #[serde(default)]
    pub blocked_mutual_invites: HashMap<i64, bool>,
    #[serde(default)]
    pub peace_treaties: Vec<PersistedPeaceTreaty>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedBattlefieldRecord {
    pub instance_id: i64,
    pub solar_system_id: u32,
    pub faction_id: i64,
    pub name: String,
    pub capture_progress: f64,
    pub completed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersistedFwState {
    #[serde(default)]
    pub system_victory_points: HashMap<u32, i64>,
    #[serde(default)]
    pub system_advantage: HashMap<u32, (f64, f64)>,
    #[serde(default)]
    pub battlefield_instances: Vec<PersistedBattlefieldRecord>,
}

/// One upgrade installed into a Sovereignty Hub (`eve_public.sovereignty.hub.upgrade`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedHubUpgrade {
    /// SDE installation type (`invTypes` row in a Sovereignty Hub upgrade group).
    pub type_id: u32,
    /// Operator-requested online flag (`UpgradeConfiguration.online`).
    pub online: bool,
    /// `true` while the upgrade is starved of fuel / power (`POWER_STATE_LOW`).
    #[serde(default)]
    pub low_power: bool,
}

/// Workforce transfer mode of a Sovereignty Hub (`eve_public.sovereignty.hub.workforce.Configuration`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PersistedWorkforceMode {
    #[default]
    Inactive,
    Import { sources: Vec<u32> },
    Export { destination: Option<u32>, amount: u64 },
    Transit,
}

/// Mutable Equinox runtime state of a Sovereignty Hub structure.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedSovHubData {
    pub hub_id: i64,
    #[serde(default)]
    pub upgrades: Vec<PersistedHubUpgrade>,
    /// Fuel bay: fuel type id -> units.
    #[serde(default)]
    pub fuel: BTreeMap<u32, u32>,
    #[serde(default)]
    pub workforce: PersistedWorkforceMode,
    /// Unix seconds of the last fuel-burn simulation / configuration change.
    #[serde(default)]
    pub last_updated_unix: u64,
}

/// Mercenary Den runtime state (`eve_public.sovereignty.mercenaryden`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedMercenaryDen {
    pub den_id: i64,
    pub type_id: u32,
    pub owner_char_id: i64,
    pub skyhook_id: i64,
    pub solar_system_id: u32,
    pub planet_id: i64,
    pub enabled: bool,
    pub cargo_extraction_enabled: bool,
    pub development_level: u32,
    pub anarchy_level: u32,
    pub infomorphs: u32,
    pub simulated_at_unix: u64,
    pub next_activity_generation_unix: u64,
}

/// Mercenary Tactical Operation offered by a den (`eve_public.sovereignty.mercenaryden.activity`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedMercenaryActivity {
    pub uuid: Vec<u8>,
    pub den_id: i64,
    pub solar_system_id: u32,
    pub dungeon_id: u32,
    pub started: bool,
    pub started_by: i64,
    pub expiry_unix: u64,
    pub development_impact: i32,
    pub anarchy_impact: i32,
    pub infomorph_bonus: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedSovState {
    #[serde(default)]
    pub fuel_access_groups: HashMap<i64, i64>,
    #[serde(default)]
    pub hacked_hubs: HashMap<i64, u64>,
    #[serde(default)]
    pub skyhook_reagents: HashMap<i64, (u32, u64)>,
    #[serde(default)]
    pub skyhook_hackable: HashMap<i64, bool>,
    /// Persisted orbital skyhook activation state (`skyhook_id -> active`), independent of
    /// `UpwellStructureRecord.state`. Defaults to `true` when absent.
    #[serde(default)]
    pub skyhook_active: BTreeMap<i64, bool>,
    /// Scheduled skyhook theft vulnerability windows: skyhook id -> (start, end) unix seconds.
    #[serde(default)]
    pub skyhook_theft_windows: HashMap<i64, (u64, u64)>,
    #[serde(default)]
    pub sov_hubs: HashMap<i64, PersistedSovHubData>,
    #[serde(default)]
    pub mercenary_dens: HashMap<i64, PersistedMercenaryDen>,
    /// Activities keyed by hex-encoded UUID.
    #[serde(default)]
    pub mercenary_activities: BTreeMap<String, PersistedMercenaryActivity>,
    #[serde(default)]
    pub next_mercenary_activity_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedInsurgencyCampaign {
    pub campaign_id: i64,
    pub pirate_faction_id: i64,
    pub target_faction_id: i64,
    pub fob_solar_system_id: u32,
    pub systems: Vec<u32>,
    pub created_at_unix: u64,
    pub forecasting_end_unix: u64,
    pub active_end_unix: u64,
    pub winddown_end_unix: u64,
    #[serde(default)]
    pub corruption_points: HashMap<u32, i64>,
    #[serde(default)]
    pub suppression_points: HashMap<u32, i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedInsurgencySiteRecord {
    pub instance_id: i64,
    pub solar_system_id: u32,
    pub site_type: String,
    pub name: String,
    pub corruption_reward: f64,
    pub completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedIceHeistRecord {
    pub instance_id: i64,
    pub solar_system_id: u32,
    pub pirate_tankers_saved: u32,
    pub empire_tankers_saved: u32,
    pub completed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersistedInsurgencyState {
    #[serde(default)]
    pub campaigns: HashMap<i64, PersistedInsurgencyCampaign>,
    #[serde(default)]
    pub insurgency_instances: Vec<PersistedInsurgencySiteRecord>,
    #[serde(default)]
    pub ice_heist_instances: Vec<PersistedIceHeistRecord>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedCrimeWatchState {
    pub character_id: i64,
    /// 60-second weapon timer: blocks docking, jumping stargates, or ejecting.
    pub weapon_timer_until_unix: u64,
    /// 15-minute (900s) PvP aggression timer: prevents safe logoff and emergency warp cloaking.
    pub pvp_timer_until_unix: u64,
    /// 15-minute (900s) Suspect flag: allows free engagement by any pilot in high-sec / low-sec.
    pub suspect_flag_until_unix: u64,
    /// 15-minute (900s) Criminal flag: free engagement everywhere + triggers CONCORD in high-sec.
    pub criminal_flag_until_unix: u64,
    /// Pending CONCORD arrival: `(system_id, arrival_unix, criminal_ship_id, victim_char_id)`.
    pub pending_concord_arrival: Option<(u32, u64, i64, i64)>,
}

impl PersistedCrimeWatchState {
    pub fn is_weapon_timer_active(&self, now_unix: u64) -> bool {
        self.weapon_timer_until_unix > now_unix
    }

    pub fn is_pvp_active(&self, now_unix: u64) -> bool {
        self.pvp_timer_until_unix > now_unix
    }

    pub fn is_suspect(&self, now_unix: u64) -> bool {
        self.suspect_flag_until_unix > now_unix
    }

    pub fn is_criminal(&self, now_unix: u64) -> bool {
        self.criminal_flag_until_unix > now_unix
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedAggressionRecord {
    pub aggressor_char_id: i64,
    pub victim_char_id: i64,
    pub system_id: u32,
    pub started_unix: u64,
    pub expires_unix: u64,
    pub is_legal: bool,
    pub is_pod: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedSecStatusTransaction {
    pub timestamp_unix: u64,
    pub old_sec: f64,
    pub new_sec: f64,
    pub delta: f64,
    pub reason: String,
    pub solar_system_id: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KillmailAttackerEntry {
    pub character_id: i64,
    pub corporation_id: i64,
    pub alliance_id: Option<i64>,
    pub ship_type_id: u32,
    pub weapon_type_id: u32,
    pub damage_done: f64,
    pub final_blow: bool,
    pub security_status: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KillmailItemEntry {
    pub type_id: u32,
    pub flag: u16,
    pub qty_dropped: i32,
    pub qty_destroyed: i32,
    pub singleton: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetailedKillmail {
    pub kill_id: i64,
    pub kill_time_unix: u64,
    pub solar_system_id: u32,
    pub victim_character_id: i64,
    pub victim_corporation_id: i64,
    pub victim_alliance_id: Option<i64>,
    pub victim_ship_type_id: u32,
    pub victim_damage_taken: f64,
    pub victim_position: [f64; 3],
    pub attackers: Vec<KillmailAttackerEntry>,
    pub dropped_items: Vec<KillmailItemEntry>,
    pub destroyed_items: Vec<KillmailItemEntry>,
    pub total_value_isk: f64,
    pub killmail_hash: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersistedCombatCrimewatchState {
    #[serde(default)]
    pub sec_status_transactions: HashMap<i64, Vec<PersistedSecStatusTransaction>>,
    #[serde(default)]
    pub safety_levels: HashMap<i64, i32>,
    #[serde(default)]
    pub drone_settings: HashMap<i64, (bool, bool)>,
    #[serde(default)]
    pub limited_engagements: HashMap<i64, HashMap<i64, u64>>,
    #[serde(default)]
    pub npc_timers: HashMap<i64, u64>,
    #[serde(default)]
    pub disapproval_flags: HashMap<i64, u64>,
    #[serde(default, with = "tuple_map_serde")]
    pub duel_challenges: HashMap<(i64, i64), u64>,
    #[serde(default)]
    pub corp_aggression_settings: HashMap<i64, bool>,
    #[serde(default)]
    pub player_bounty_pool: HashMap<i64, f64>,
    #[serde(default)]
    pub bounty_contributions: HashMap<i64, Vec<(i64, f64)>>,
    #[serde(default)]
    pub bounty_hunter_claimed: HashMap<i64, f64>,
    #[serde(default)]
    pub kill_right_expires_unix: HashMap<i64, u64>,
    #[serde(default)]
    pub kill_right_restricted_to: HashMap<i64, i64>,
    #[serde(default)]
    pub detailed_killmails: HashMap<i64, DetailedKillmail>,
}
