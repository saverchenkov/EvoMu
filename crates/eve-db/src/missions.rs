//! Mission instance model, typed objectives, single-source kind classification,
//! and Copy-on-Write indexed storage (`#1496` Track 3).

use eve_sde::{AgentInfo, MissionTemplate, SdeRegistry, SolarSystemId, TypeId};
use imbl::OrdMap;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::de::{Deserializer, Error as DeError};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

/// Unique per-instance identifier for a character's mission (`#1496`).
pub type MissionInstanceId = i64;

/// Lifecycle state of a mission instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MissionState {
    #[serde(rename = "Offered")]
    Offered,
    #[serde(rename = "Accepted")]
    Accepted,
    #[serde(rename = "Completed")]
    Completed,
    #[serde(rename = "Expired")]
    Expired,
    #[serde(rename = "Failed")]
    Failed,
    #[serde(rename = "Quit")]
    Quit,
    #[serde(rename = "Declined")]
    Declined,
}

impl MissionState {
    /// Canonical persisted string representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Offered => "Offered",
            Self::Accepted => "Accepted",
            Self::Completed => "Completed",
            Self::Expired => "Expired",
            Self::Failed => "Failed",
            Self::Quit => "Quit",
            Self::Declined => "Declined",
        }
    }

    /// Strict parser accepting only canonical persisted mission state strings.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "Offered" => Some(Self::Offered),
            "Accepted" => Some(Self::Accepted),
            "Completed" => Some(Self::Completed),
            "Expired" => Some(Self::Expired),
            "Failed" => Some(Self::Failed),
            "Quit" => Some(Self::Quit),
            "Declined" => Some(Self::Declined),
            _ => None,
        }
    }

    /// Numeric V24 EVE client `missionState` code (`GetMyJournalDetails` / `GetInfo`).
    pub const fn client_state_num(self) -> i64 {
        match self {
            Self::Offered => 1,
            Self::Accepted => 2,
            Self::Failed | Self::Expired => 3,
            Self::Completed => 4,
            Self::Declined => 5,
            Self::Quit => 6,
        }
    }

    /// Returns `true` when the mission is currently Offered or Accepted.
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Offered | Self::Accepted)
    }
}

impl fmt::Display for MissionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Gameplay classification of a mission instance decided once on offer (`#1496`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MissionKind {
    Encounter,
    Courier,
    Trade,
    Mining,
    Heraldry,
    DataCenterTurnIn,
    ResearchTurnIn,
}

impl MissionKind {
    /// Journal bookmark hint string expected by the V24 client.
    pub const fn journal_mission_type_hint(self) -> &'static str {
        match self {
            Self::Trade | Self::DataCenterTurnIn | Self::ResearchTurnIn => {
                "UI/Agents/MissionTypes/Trade"
            }
            Self::Courier => "UI/Agents/MissionTypes/Courier",
            Self::Mining => "UI/Agents/MissionTypes/Mining",
            Self::Encounter | Self::Heraldry => "UI/Agents/MissionTypes/Encounter",
        }
    }

    /// Journal bookmark `locationType` label expected by the V24 client.
    pub const fn journal_location_type(self) -> &'static str {
        match self {
            Self::Courier | Self::Trade | Self::DataCenterTurnIn | Self::ResearchTurnIn => {
                "objective.destination"
            }
            Self::Encounter | Self::Mining | Self::Heraldry => "dungeon",
        }
    }
}

/// Exhaustive catalog of SDE `content_template` identifiers in `sde_missions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MissionContentType {
    BasicKillMission,
    StorylineKillMission,
    GenericStorylineKillMission,
    EpicArcKillMission,
    BasicCourierMission,
    StorylineCourierMission,
    GenericStorylineCourierMission,
    EpicArcCourierMission,
    ResearchCourierMission,
    BasicTradeMission,
    StorylineTradeMission,
    GenericStorylineTradeMission,
    EpicArcTradeMission,
    ResearchTradeMission,
    BasicMiningMission,
    HeraldryMission,
    EpicArcTalkToAgentMission,
    EpicArcAgentInteractionMission,
}

impl MissionContentType {
    /// All 18 known SDE `content_template` variants.
    pub const ALL: [Self; 18] = [
        Self::BasicKillMission,
        Self::StorylineKillMission,
        Self::GenericStorylineKillMission,
        Self::EpicArcKillMission,
        Self::BasicCourierMission,
        Self::StorylineCourierMission,
        Self::GenericStorylineCourierMission,
        Self::EpicArcCourierMission,
        Self::ResearchCourierMission,
        Self::BasicTradeMission,
        Self::StorylineTradeMission,
        Self::GenericStorylineTradeMission,
        Self::EpicArcTradeMission,
        Self::ResearchTradeMission,
        Self::BasicMiningMission,
        Self::HeraldryMission,
        Self::EpicArcTalkToAgentMission,
        Self::EpicArcAgentInteractionMission,
    ];

    /// Parses an exact SDE `content_template` string, rejecting unknown variants.
    pub fn from_sde_str(raw: &str) -> Result<Self, String> {
        match raw {
            "agent.missionTemplatizedContent_BasicKillMission" => Ok(Self::BasicKillMission),
            "agent.missionTemplatizedContent_StorylineKillMission" => {
                Ok(Self::StorylineKillMission)
            }
            "agent.missionTemplatizedContent_GenericStorylineKillMission" => {
                Ok(Self::GenericStorylineKillMission)
            }
            "agent.missionTemplatizedContent_EpicArcKillMission" => Ok(Self::EpicArcKillMission),
            "agent.missionTemplatizedContent_BasicCourierMission" => Ok(Self::BasicCourierMission),
            "agent.missionTemplatizedContent_StorylineCourierMission" => {
                Ok(Self::StorylineCourierMission)
            }
            "agent.missionTemplatizedContent_GenericStorylineCourierMission" => {
                Ok(Self::GenericStorylineCourierMission)
            }
            "agent.missionTemplatizedContent_EpicArcCourierMission" => {
                Ok(Self::EpicArcCourierMission)
            }
            "agent.missionTemplatizedContent_ResearchCourierMission" => {
                Ok(Self::ResearchCourierMission)
            }
            "agent.missionTemplatizedContent_BasicTradeMission" => Ok(Self::BasicTradeMission),
            "agent.missionTemplatizedContent_StorylineTradeMission" => {
                Ok(Self::StorylineTradeMission)
            }
            "agent.missionTemplatizedContent_GenericStorylineTradeMission" => {
                Ok(Self::GenericStorylineTradeMission)
            }
            "agent.missionTemplatizedContent_EpicArcTradeMission" => Ok(Self::EpicArcTradeMission),
            "agent.missionTemplatizedContent_ResearchTradeMission" => {
                Ok(Self::ResearchTradeMission)
            }
            "agent.missionTemplatizedContent_BasicMiningMission" => Ok(Self::BasicMiningMission),
            "agent.missionTemplatizedContent_HeraldryMission" => Ok(Self::HeraldryMission),
            "agent.missionTemplatizedContent_EpicArcTalkToAgentMission" => {
                Ok(Self::EpicArcTalkToAgentMission)
            }
            "agent.missionTemplatizedContent_EpicArcAgentInteractionMission" => {
                Ok(Self::EpicArcAgentInteractionMission)
            }
            other => Err(format!("unknown SDE mission content_template '{other}'")),
        }
    }

    /// Canonical SDE `content_template` string for this variant.
    pub const fn as_sde_str(self) -> &'static str {
        match self {
            Self::BasicKillMission => "agent.missionTemplatizedContent_BasicKillMission",
            Self::StorylineKillMission => "agent.missionTemplatizedContent_StorylineKillMission",
            Self::GenericStorylineKillMission => {
                "agent.missionTemplatizedContent_GenericStorylineKillMission"
            }
            Self::EpicArcKillMission => "agent.missionTemplatizedContent_EpicArcKillMission",
            Self::BasicCourierMission => "agent.missionTemplatizedContent_BasicCourierMission",
            Self::StorylineCourierMission => {
                "agent.missionTemplatizedContent_StorylineCourierMission"
            }
            Self::GenericStorylineCourierMission => {
                "agent.missionTemplatizedContent_GenericStorylineCourierMission"
            }
            Self::EpicArcCourierMission => "agent.missionTemplatizedContent_EpicArcCourierMission",
            Self::ResearchCourierMission => {
                "agent.missionTemplatizedContent_ResearchCourierMission"
            }
            Self::BasicTradeMission => "agent.missionTemplatizedContent_BasicTradeMission",
            Self::StorylineTradeMission => "agent.missionTemplatizedContent_StorylineTradeMission",
            Self::GenericStorylineTradeMission => {
                "agent.missionTemplatizedContent_GenericStorylineTradeMission"
            }
            Self::EpicArcTradeMission => "agent.missionTemplatizedContent_EpicArcTradeMission",
            Self::ResearchTradeMission => "agent.missionTemplatizedContent_ResearchTradeMission",
            Self::BasicMiningMission => "agent.missionTemplatizedContent_BasicMiningMission",
            Self::HeraldryMission => "agent.missionTemplatizedContent_HeraldryMission",
            Self::EpicArcTalkToAgentMission => {
                "agent.missionTemplatizedContent_EpicArcTalkToAgentMission"
            }
            Self::EpicArcAgentInteractionMission => {
                "agent.missionTemplatizedContent_EpicArcAgentInteractionMission"
            }
        }
    }
}

/// Decides `MissionKind` from a catalogued [`MissionContentType`], matching exact offer precedence:
/// `ResearchTurnIn`, `DataCenterTurnIn`, `Trade`, `Heraldry`, `Courier`, `Mining`, `Encounter`.
pub fn decide_mission_kind_from_known(
    content: MissionContentType,
    tmpl: &MissionTemplate,
    agent: Option<&AgentInfo>,
) -> MissionKind {
    if agent.is_some_and(|a| a.agent_type_id == 4) {
        return MissionKind::ResearchTurnIn;
    }
    if agent.is_some_and(|a| a.agent_type_id == 5) {
        return MissionKind::DataCenterTurnIn;
    }
    match content {
        MissionContentType::BasicTradeMission
        | MissionContentType::StorylineTradeMission
        | MissionContentType::GenericStorylineTradeMission
        | MissionContentType::EpicArcTradeMission
        | MissionContentType::ResearchTradeMission => MissionKind::Trade,
        MissionContentType::HeraldryMission => MissionKind::Heraldry,
        MissionContentType::BasicCourierMission
        | MissionContentType::StorylineCourierMission
        | MissionContentType::GenericStorylineCourierMission
        | MissionContentType::EpicArcCourierMission
        | MissionContentType::ResearchCourierMission => MissionKind::Courier,
        MissionContentType::BasicMiningMission => MissionKind::Mining,
        MissionContentType::BasicKillMission
        | MissionContentType::StorylineKillMission
        | MissionContentType::GenericStorylineKillMission
        | MissionContentType::EpicArcKillMission => MissionKind::Encounter,
        MissionContentType::EpicArcTalkToAgentMission
        | MissionContentType::EpicArcAgentInteractionMission => {
            if tmpl.is_courier {
                MissionKind::Courier
            } else if tmpl.title.to_lowercase().contains("mining")
                || agent.is_some_and(|a| a.division_id == 23)
            {
                MissionKind::Mining
            } else {
                MissionKind::Encounter
            }
        }
    }
}

/// Strict SDE-validated mission kind classification; errors on uncatalogued or missing `content_template`.
pub fn decide_mission_kind_strict(
    tmpl: &MissionTemplate,
    agent: Option<&AgentInfo>,
) -> Result<MissionKind, String> {
    let raw = tmpl
        .content_template
        .as_deref()
        .ok_or_else(|| format!("mission {} missing content_template", tmpl.mission_id))?;
    let content = MissionContentType::from_sde_str(raw)?;
    Ok(decide_mission_kind_from_known(content, tmpl, agent))
}

/// Single-source mission kind classification used at offer time (and for synthetic test templates).
pub fn decide_mission_kind(
    tmpl: Option<&MissionTemplate>,
    agent: Option<&AgentInfo>,
) -> MissionKind {
    if let Some(t) = tmpl {
        if let Some(raw) = t.content_template.as_deref() {
            if let Ok(content) = MissionContentType::from_sde_str(raw) {
                return decide_mission_kind_from_known(content, t, agent);
            }
        }
    }
    if agent.is_some_and(|a| a.agent_type_id == 4) {
        return MissionKind::ResearchTurnIn;
    }
    if agent.is_some_and(|a| a.agent_type_id == 5) {
        return MissionKind::DataCenterTurnIn;
    }
    let ct = tmpl
        .and_then(|t| t.content_template.as_deref())
        .unwrap_or_default();
    if ct.contains("Trade") {
        return MissionKind::Trade;
    }
    if ct.contains("Heraldry") {
        return MissionKind::Heraldry;
    }
    if ct.contains("Courier") || tmpl.is_some_and(|t| t.is_courier) {
        return MissionKind::Courier;
    }
    let is_kill = ct.contains("BasicKill") || ct.contains("Kill") || ct.contains("Encounter");
    if !is_kill
        && (ct.contains("BasicMining")
            || tmpl.is_some_and(|t| t.title.to_lowercase().contains("mining"))
            || agent.is_some_and(|a| a.division_id == 23))
    {
        return MissionKind::Mining;
    }
    MissionKind::Encounter
}

/// Combat / mining deadspace encounter objective state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EncounterObjective {
    pub npcs_remaining: u32,
    #[serde(default)]
    pub spawned_npc_ball_ids: Vec<i64>,
    #[serde(default)]
    pub initial_wave_count: Option<u32>,
    #[serde(default)]
    pub initial_wave_types: Vec<TypeId>,
    #[serde(default)]
    pub awaiting_downtime_respawn: bool,
}

/// Station-to-station courier cargo objective state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CourierObjective {
    #[serde(default)]
    pub cargo_type_id: Option<TypeId>,
    #[serde(default)]
    pub quantity: i32,
    #[serde(default)]
    pub destination_station_id: Option<i64>,
    #[serde(default)]
    pub granted_item_id: Option<i64>,
    #[serde(default)]
    pub granted_qty: i32,
}

/// Item acquisition / turn-in objective state (`Trade`, `Mining`, `DataCenterTurnIn`, `ResearchTurnIn`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeliveryObjective {
    #[serde(default)]
    pub item_type_id: Option<TypeId>,
    #[serde(default)]
    pub quantity: i32,
}

/// Typed mission objective payload stored on [`MissionInstance`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MissionObjective {
    Encounter(EncounterObjective),
    Courier(CourierObjective),
    Delivery(DeliveryObjective),
    None,
}

impl MissionObjective {
    /// Returns the default objective variant matching `kind`.
    pub fn default_for_kind(kind: MissionKind) -> Self {
        match kind {
            MissionKind::Encounter => Self::Encounter(EncounterObjective::default()),
            MissionKind::Courier => Self::Courier(CourierObjective::default()),
            MissionKind::Trade
            | MissionKind::Mining
            | MissionKind::DataCenterTurnIn
            | MissionKind::ResearchTurnIn => Self::Delivery(DeliveryObjective::default()),
            MissionKind::Heraldry => Self::None,
        }
    }
}

/// Persisted per-instance mission record unifying active row, timing/cargo metadata,
/// and downtime wave tracking (`#1496` Track 3).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MissionInstance {
    pub instance_id: MissionInstanceId,
    pub character_id: i64,
    pub agent_id: i64,
    pub template_id: u32,
    pub state: MissionState,
    pub kind: MissionKind,
    #[serde(default)]
    pub solar_system_id: SolarSystemId,
    pub current_room_id: u32,
    #[serde(default)]
    pub bookmark_id: Option<i64>,
    #[serde(default)]
    pub origin_station_id: Option<i64>,
    #[serde(default)]
    pub offered_level: Option<u8>,
    #[serde(default)]
    pub is_anomic_burner: bool,
    #[serde(default)]
    pub offered_at_unix: u64,
    #[serde(default)]
    pub offer_expiry_unix: u64,
    #[serde(default)]
    pub accepted_at_unix: Option<u64>,
    #[serde(default)]
    pub accept_expiry_unix: Option<u64>,
    #[serde(default)]
    pub bonus_expiry_unix: Option<u64>,
    pub objective: MissionObjective,
}

impl MissionInstance {
    /// Verifies that `self.kind` agrees with the `self.objective` variant.
    pub fn is_objective_consistent(&self) -> bool {
        matches!(
            (self.kind, &self.objective),
            (MissionKind::Encounter, MissionObjective::Encounter(_))
                | (MissionKind::Courier, MissionObjective::Courier(_))
                | (
                    MissionKind::Trade
                        | MissionKind::Mining
                        | MissionKind::DataCenterTurnIn
                        | MissionKind::ResearchTurnIn,
                    MissionObjective::Delivery(_),
                )
                | (MissionKind::Heraldry, MissionObjective::None)
        )
    }

    pub fn encounter(&self) -> Option<&EncounterObjective> {
        match &self.objective {
            MissionObjective::Encounter(enc) => Some(enc),
            _ => None,
        }
    }

    pub fn encounter_mut(&mut self) -> Option<&mut EncounterObjective> {
        match &mut self.objective {
            MissionObjective::Encounter(enc) => Some(enc),
            _ => None,
        }
    }

    pub fn courier(&self) -> Option<&CourierObjective> {
        match &self.objective {
            MissionObjective::Courier(c) => Some(c),
            _ => None,
        }
    }

    pub fn courier_mut(&mut self) -> Option<&mut CourierObjective> {
        match &mut self.objective {
            MissionObjective::Courier(c) => Some(c),
            _ => None,
        }
    }

    pub fn delivery(&self) -> Option<&DeliveryObjective> {
        match &self.objective {
            MissionObjective::Delivery(d) => Some(d),
            _ => None,
        }
    }

    pub fn delivery_mut(&mut self) -> Option<&mut DeliveryObjective> {
        match &mut self.objective {
            MissionObjective::Delivery(d) => Some(d),
            _ => None,
        }
    }

    pub fn npcs_remaining(&self) -> u32 {
        self.encounter().map_or(0, |e| e.npcs_remaining)
    }

    pub fn spawned_npc_ball_ids(&self) -> &[i64] {
        self.encounter()
            .map_or(&[], |e| e.spawned_npc_ball_ids.as_slice())
    }

    pub fn destination_station_id(&self) -> Option<i64> {
        self.courier().and_then(|c| c.destination_station_id)
    }

    pub fn granted_courier(&self) -> Option<(Option<i64>, i32)> {
        self.courier()
            .map(|c| (c.granted_item_id, c.granted_qty))
    }

    /// Returns `(required_type_id, required_qty)` if the objective carries an item type requirement.
    pub fn required_delivery(&self) -> Option<(TypeId, i32)> {
        match &self.objective {
            MissionObjective::Courier(c) => c.cargo_type_id.map(|tid| (tid, c.quantity)),
            MissionObjective::Delivery(d) => d.item_type_id.map(|tid| (tid, d.quantity)),
            MissionObjective::Encounter(_) | MissionObjective::None => None,
        }
    }

    pub fn initial_wave_count(&self) -> Option<u32> {
        self.encounter().and_then(|e| e.initial_wave_count)
    }

    pub fn initial_wave_types(&self) -> &[TypeId] {
        self.encounter()
            .map_or(&[], |e| e.initial_wave_types.as_slice())
    }

    pub fn awaiting_downtime_respawn(&self) -> bool {
        self.encounter()
            .is_some_and(|e| e.awaiting_downtime_respawn)
    }
}

fn dedup_preserve_order(ids: &mut Vec<i64>) {
    if ids.len() <= 1 {
        return;
    }
    let mut seen = BTreeSet::new();
    ids.retain(|&id| seen.insert(id));
}

/// Opens a read-only SQLite handle to the SDE database if discoverable from `EVOMU_SDE_PATH`
/// or repository ancestors (`data/evomu_sde.sqlite`).
pub fn open_sde_sqlite_ro() -> Option<Connection> {
    let default_path = SdeRegistry::default_db_path();
    let path = if default_path.exists() {
        Some(default_path)
    } else {
        let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut found = None;
        for _ in 0..6 {
            let candidate = dir.join("data/evomu_sde.sqlite");
            if candidate.exists() {
                found = Some(candidate);
                break;
            }
            if !dir.pop() {
                break;
            }
        }
        found
    }?;
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
}

/// Fetches minimal SDE `MissionTemplate` and `AgentInfo` rows by primary key for legacy
/// conversion and fixture kind resolution without loading the full `SdeRegistry`.
pub fn lookup_sde_mission_and_agent(
    conn: &Connection,
    mission_id: u32,
    agent_id: i64,
) -> (Option<MissionTemplate>, Option<AgentInfo>) {
    let tmpl = conn
        .query_row(
            "SELECT mission_id, name, is_courier, lp_alpha, lp_omega, has_standing_rewards, content_template FROM sde_missions WHERE mission_id = ?1",
            [i64::from(mission_id)],
            |r| {
                Ok(MissionTemplate {
                    mission_id: r.get::<_, i64>(0)? as u32,
                    title: r.get(1)?,
                    is_courier: r.get::<_, i64>(2)? != 0,
                    lp_alpha: r.get(3)?,
                    lp_omega: r.get(4)?,
                    has_standing_rewards: r.get::<_, i64>(5)? != 0,
                    content_template: r.get(6)?,
                    dungeon_id: None,
                })
            },
        )
        .optional()
        .ok()
        .flatten();
    let agent = conn
        .query_row(
            "SELECT agent_id, name, corporation_id, station_id, solar_system_id, level, division_id, agent_type_id, is_locator FROM sde_agents WHERE agent_id = ?1",
            [agent_id],
            |r| {
                Ok(AgentInfo {
                    agent_id: r.get(0)?,
                    name: r.get(1)?,
                    corporation_id: r.get(2)?,
                    station_id: r.get(3)?,
                    solar_system_id: r.get::<_, Option<i64>>(4)?.map(|v| v as SolarSystemId),
                    level: r.get::<_, i64>(5)? as u8,
                    division_id: r.get::<_, i64>(6)? as u32,
                    agent_type_id: r.get::<_, i64>(7)? as u32,
                    is_locator: r.get::<_, i64>(8)? != 0,
                    research_skills: Vec::new(),
                })
            },
        )
        .optional()
        .ok()
        .flatten();
    (tmpl, agent)
}

fn validate_instance(instance: &mut MissionInstance) -> Result<(), String> {
    if !instance.is_objective_consistent() {
        return Err(format!(
            "mission instance {} has incompatible kind {:?} and objective variant",
            instance.instance_id, instance.kind
        ));
    }
    if let MissionObjective::Encounter(enc) = &mut instance.objective {
        dedup_preserve_order(&mut enc.spawned_npc_ball_ids);
    }
    Ok(())
}

/// CoW indexed storage of all [`MissionInstance`] records in [`crate::WorldStateSnapshot`].
///
/// Invariants:
/// - Primary key is `instance_id` (`MissionInstanceId`), iterated deterministically in ascending order.
/// - Track 3 enforces unique `(character_id, template_id)` across all stored instances.
/// - Secondary indexes (`by_character`, `by_char_template`, `by_npc_ball`) stay synchronized
///   across every checked mutation with zero dirty flags or reconciliation passes.
#[derive(Debug, Clone, Default)]
pub struct MissionInstanceStore {
    by_id: OrdMap<MissionInstanceId, MissionInstance>,
    by_character: OrdMap<(i64, MissionInstanceId), ()>,
    by_char_template: OrdMap<(i64, u32), MissionInstanceId>,
    by_npc_ball: OrdMap<i64, MissionInstanceId>,
}

impl PartialEq for MissionInstanceStore {
    fn eq(&self, other: &Self) -> bool {
        self.by_id.ptr_eq(&other.by_id) || self.by_id == other.by_id
    }
}

impl Eq for MissionInstanceStore {}

impl MissionInstanceStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuilds a validated store from an iterator of mission instances.
    pub fn from_instances<I>(iter: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = MissionInstance>,
    {
        let mut store = Self::new();
        for item in iter {
            store.try_insert(item)?;
        }
        Ok(store)
    }

    /// Returns `true` when two stores share the exact same underlying primary B-tree root.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        self.by_id.ptr_eq(&other.by_id)
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn get(&self, instance_id: MissionInstanceId) -> Option<&MissionInstance> {
        self.by_id.get(&instance_id)
    }

    pub fn contains_instance_id(&self, instance_id: MissionInstanceId) -> bool {
        self.by_id.contains_key(&instance_id)
    }

    pub fn get_by_character_template(
        &self,
        character_id: i64,
        template_id: u32,
    ) -> Option<&MissionInstance> {
        self.by_char_template
            .get(&(character_id, template_id))
            .and_then(|id| self.by_id.get(id))
    }

    pub fn contains_character_template(&self, character_id: i64, template_id: u32) -> bool {
        self.by_char_template
            .contains_key(&(character_id, template_id))
    }

    pub fn contains_spawned_npc_id(&self, ball_id: i64) -> bool {
        self.by_npc_ball.contains_key(&ball_id)
    }

    pub fn get_by_npc_ball(&self, ball_id: i64) -> Option<&MissionInstance> {
        self.by_npc_ball
            .get(&ball_id)
            .and_then(|id| self.by_id.get(id))
    }

    /// Deterministic ascending iteration by `MissionInstanceId`.
    pub fn iter(&self) -> impl Iterator<Item = &MissionInstance> + '_ {
        self.by_id.values()
    }

    /// Indexed `O(log N + k)` iteration over one character's missions in ascending `MissionInstanceId` order.
    pub fn iter_by_character(&self, character_id: i64) -> impl Iterator<Item = &MissionInstance> + '_ {
        self.by_character
            .range((character_id, i64::MIN)..=(character_id, i64::MAX))
            .filter_map(|(&(_, instance_id), _)| self.by_id.get(&instance_id))
    }

    /// Deterministic ascending `MissionInstanceId` keys.
    pub fn keys(&self) -> impl Iterator<Item = MissionInstanceId> + '_ {
        self.by_id.keys().copied()
    }

    /// All live indexed NPC ball IDs across stored encounter objectives.
    pub fn spawned_npc_ball_ids(&self) -> impl Iterator<Item = i64> + '_ {
        self.by_npc_ball.keys().copied()
    }

    fn insert_validated_unchecked(&mut self, instance: MissionInstance) {
        let id = instance.instance_id;
        let char_id = instance.character_id;
        let tmpl_id = instance.template_id;
        for &ball_id in instance.spawned_npc_ball_ids() {
            self.by_npc_ball.insert(ball_id, id);
        }
        self.by_character.insert((char_id, id), ());
        self.by_char_template.insert((char_id, tmpl_id), id);
        self.by_id.insert(id, instance);
    }

    fn deindex_existing(&mut self, old: &MissionInstance) {
        self.by_character.remove(&(old.character_id, old.instance_id));
        self.by_char_template
            .remove(&(old.character_id, old.template_id));
        for &ball_id in old.spawned_npc_ball_ids() {
            if self.by_npc_ball.get(&ball_id) == Some(&old.instance_id) {
                self.by_npc_ball.remove(&ball_id);
            }
        }
    }

    /// Inserts a new [`MissionInstance`], failing on duplicate `instance_id`, duplicate
    /// `(character_id, template_id)`, or NPC ball ID collision with another instance.
    pub fn try_insert(&mut self, mut instance: MissionInstance) -> Result<(), String> {
        validate_instance(&mut instance)?;
        if self.by_id.contains_key(&instance.instance_id) {
            return Err(format!(
                "duplicate mission instance_id {}",
                instance.instance_id
            ));
        }
        if let Some(&existing_id) = self
            .by_char_template
            .get(&(instance.character_id, instance.template_id))
        {
            return Err(format!(
                "duplicate mission for character {} and template {} (existing instance {})",
                instance.character_id, instance.template_id, existing_id
            ));
        }
        for &ball_id in instance.spawned_npc_ball_ids() {
            if let Some(&owner) = self.by_npc_ball.get(&ball_id) {
                return Err(format!(
                    "NPC ball_id {ball_id} already owned by mission instance {owner}"
                ));
            }
        }
        self.insert_validated_unchecked(instance);
        Ok(())
    }

    /// Inserts or replaces the row matching `(instance.character_id, instance.template_id)`.
    pub fn upsert_by_character_template(
        &mut self,
        mut instance: MissionInstance,
    ) -> Result<Option<MissionInstance>, String> {
        validate_instance(&mut instance)?;
        let replaced_id = self
            .by_char_template
            .get(&(instance.character_id, instance.template_id))
            .copied();
        if let Some(existing_with_pk) = self.by_id.get(&instance.instance_id) {
            if Some(existing_with_pk.instance_id) != replaced_id {
                return Err(format!(
                    "mission instance_id {} already belongs to ({}, {})",
                    instance.instance_id,
                    existing_with_pk.character_id,
                    existing_with_pk.template_id
                ));
            }
        }
        for &ball_id in instance.spawned_npc_ball_ids() {
            if let Some(&owner) = self.by_npc_ball.get(&ball_id) {
                if Some(owner) != replaced_id && owner != instance.instance_id {
                    return Err(format!(
                        "NPC ball_id {ball_id} already owned by mission instance {owner}"
                    ));
                }
            }
        }
        let old = replaced_id.and_then(|old_id| self.remove(old_id));
        self.insert_validated_unchecked(instance);
        Ok(old)
    }

    /// Removes a mission instance by `instance_id`, updating all secondary indexes.
    pub fn remove(&mut self, instance_id: MissionInstanceId) -> Option<MissionInstance> {
        let old = self.by_id.remove(&instance_id)?;
        self.deindex_existing(&old);
        Some(old)
    }

    /// Removes a mission instance by `(character_id, template_id)`.
    pub fn remove_by_character_template(
        &mut self,
        character_id: i64,
        template_id: u32,
    ) -> Option<MissionInstance> {
        let id = self
            .by_char_template
            .get(&(character_id, template_id))
            .copied()?;
        self.remove(id)
    }

    /// Removes all mission instances without touching allocation if already empty.
    pub fn clear(&mut self) {
        if self.by_id.is_empty()
            && self.by_character.is_empty()
            && self.by_char_template.is_empty()
            && self.by_npc_ball.is_empty()
        {
            return;
        }
        self.by_id.clear();
        self.by_character.clear();
        self.by_char_template.clear();
        self.by_npc_ball.clear();
    }

    /// Checked mutation of one mission by `instance_id`; preserves primary sharing if unchanged.
    pub fn update<R>(
        &mut self,
        instance_id: MissionInstanceId,
        f: impl FnOnce(&mut MissionInstance) -> R,
    ) -> Result<Option<R>, String> {
        let Some(existing) = self.by_id.get(&instance_id) else {
            return Ok(None);
        };
        let mut updated = existing.clone();
        let ret = f(&mut updated);
        if updated.instance_id != existing.instance_id {
            return Err(format!(
                "cannot mutate immutable mission instance_id from {} to {}",
                existing.instance_id, updated.instance_id
            ));
        }
        validate_instance(&mut updated)?;
        if &updated == existing {
            return Ok(Some(ret));
        }
        if (updated.character_id, updated.template_id)
            != (existing.character_id, existing.template_id)
        {
            if let Some(&conflict) = self
                .by_char_template
                .get(&(updated.character_id, updated.template_id))
            {
                if conflict != instance_id {
                    return Err(format!(
                        "duplicate mission for character {} and template {} (existing instance {})",
                        updated.character_id, updated.template_id, conflict
                    ));
                }
            }
        }
        for &ball_id in updated.spawned_npc_ball_ids() {
            if let Some(&owner) = self.by_npc_ball.get(&ball_id) {
                if owner != instance_id {
                    return Err(format!(
                        "NPC ball_id {ball_id} already owned by mission instance {owner}"
                    ));
                }
            }
        }
        let old = self.by_id.get(&instance_id).cloned().expect("checked above");
        self.deindex_existing(&old);
        self.insert_validated_unchecked(updated);
        Ok(Some(ret))
    }

    /// Checked mutation by `(character_id, template_id)`.
    pub fn update_by_character_template<R>(
        &mut self,
        character_id: i64,
        template_id: u32,
        f: impl FnOnce(&mut MissionInstance) -> R,
    ) -> Result<Option<R>, String> {
        let Some(&instance_id) = self.by_char_template.get(&(character_id, template_id)) else {
            return Ok(None);
        };
        self.update(instance_id, f)
    }

    /// Kill hook for mission NPC destruction.
    ///
    /// Looks up `npc_ball_id` in the `by_npc_ball` index first and performs zero clones or
    /// mutations when `npc_ball_id` is absent or belongs to a non-`Accepted` / wrong-system mission.
    pub fn on_npc_destroyed(&mut self, solar_system_id: SolarSystemId, npc_ball_id: i64) -> bool {
        let Some(&instance_id) = self.by_npc_ball.get(&npc_ball_id) else {
            return false;
        };
        let Some(existing) = self.by_id.get(&instance_id) else {
            return false;
        };
        if existing.state != MissionState::Accepted || existing.solar_system_id != solar_system_id {
            return false;
        }
        let mut updated = existing.clone();
        let removed = match &mut updated.objective {
            MissionObjective::Encounter(enc) => {
                let before_len = enc.spawned_npc_ball_ids.len();
                enc.spawned_npc_ball_ids.retain(|&id| id != npc_ball_id);
                if enc.spawned_npc_ball_ids.len() == before_len {
                    false
                } else {
                    enc.npcs_remaining = enc.npcs_remaining.saturating_sub(1);
                    true
                }
            }
            _ => false,
        };
        if !removed {
            return false;
        }
        self.by_npc_ball.remove(&npc_ball_id);
        self.by_id.insert(instance_id, updated);
        true
    }

    /// Asserts exact 1-to-1 bijection across primary storage and all secondary indexes.
    pub fn assert_invariants(&self) -> Result<(), String> {
        if self.by_character.len() != self.by_id.len() {
            return Err(format!(
                "by_character len {} != by_id len {}",
                self.by_character.len(),
                self.by_id.len()
            ));
        }
        if self.by_char_template.len() != self.by_id.len() {
            return Err(format!(
                "by_char_template len {} != by_id len {}",
                self.by_char_template.len(),
                self.by_id.len()
            ));
        }
        let mut expected_balls = 0usize;
        for (&id, inst) in &self.by_id {
            if inst.instance_id != id {
                return Err(format!("key {id} != instance_id {}", inst.instance_id));
            }
            if !inst.is_objective_consistent() {
                return Err(format!("inconsistent kind/objective on instance {id}"));
            }
            if !self.by_character.contains_key(&(inst.character_id, id)) {
                return Err(format!(
                    "missing by_character entry ({}, {id})",
                    inst.character_id
                ));
            }
            if self
                .by_char_template
                .get(&(inst.character_id, inst.template_id))
                != Some(&id)
            {
                return Err(format!(
                    "by_char_template mismatch for ({}, {})",
                    inst.character_id, inst.template_id
                ));
            }
            for &bid in inst.spawned_npc_ball_ids() {
                expected_balls += 1;
                if self.by_npc_ball.get(&bid) != Some(&id) {
                    return Err(format!("by_npc_ball mismatch for ball {bid}"));
                }
            }
        }
        if self.by_npc_ball.len() != expected_balls {
            return Err(format!(
                "by_npc_ball len {} != expected {expected_balls}",
                self.by_npc_ball.len()
            ));
        }
        Ok(())
    }
}

impl Serialize for MissionInstanceStore {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let items: Vec<&MissionInstance> = self.iter().collect();
        items.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for MissionInstanceStore {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let items = Vec::<MissionInstance>::deserialize(deserializer)?;
        Self::from_instances(items).map_err(DeError::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn sample_instance(
        instance_id: MissionInstanceId,
        character_id: i64,
        template_id: u32,
        state: MissionState,
        kind: MissionKind,
    ) -> MissionInstance {
        let objective = match kind {
            MissionKind::Encounter => MissionObjective::Encounter(EncounterObjective {
                npcs_remaining: 4,
                spawned_npc_ball_ids: vec![instance_id * 10 + 1, instance_id * 10 + 2],
                initial_wave_count: Some(4),
                initial_wave_types: vec![587, 588],
                awaiting_downtime_respawn: false,
            }),
            MissionKind::Courier => MissionObjective::Courier(CourierObjective {
                cargo_type_id: Some(3810),
                quantity: 12,
                destination_station_id: Some(60_003_761),
                granted_item_id: Some(instance_id * 100 + 7),
                granted_qty: 12,
            }),
            MissionKind::Trade
            | MissionKind::Mining
            | MissionKind::DataCenterTurnIn
            | MissionKind::ResearchTurnIn => MissionObjective::Delivery(DeliveryObjective {
                item_type_id: Some(match kind {
                    MissionKind::Mining => 1230,
                    MissionKind::DataCenterTurnIn => 17202,
                    MissionKind::ResearchTurnIn => 34,
                    _ => 3683,
                }),
                quantity: 25,
            }),
            MissionKind::Heraldry => MissionObjective::None,
        };
        MissionInstance {
            instance_id,
            character_id,
            agent_id: 3_018_418,
            template_id,
            state,
            kind,
            solar_system_id: 30_000_142,
            current_room_id: 1,
            bookmark_id: Some(instance_id + 1_000),
            origin_station_id: Some(60_003_760),
            offered_level: Some(3),
            is_anomic_burner: kind == MissionKind::Encounter,
            offered_at_unix: 1_700_000_000,
            offer_expiry_unix: 1_700_086_400,
            accepted_at_unix: (state != MissionState::Offered).then_some(1_700_000_100),
            accept_expiry_unix: (state != MissionState::Offered).then_some(1_700_604_800),
            bonus_expiry_unix: (state != MissionState::Offered).then_some(1_700_007_200),
            objective,
        }
    }

    #[test]
    fn mission_instance_roundtrip_all_kinds_and_states() {
        let all_states = [
            MissionState::Offered,
            MissionState::Accepted,
            MissionState::Completed,
            MissionState::Expired,
            MissionState::Failed,
            MissionState::Quit,
            MissionState::Declined,
        ];
        let all_kinds = [
            MissionKind::Encounter,
            MissionKind::Courier,
            MissionKind::Trade,
            MissionKind::Mining,
            MissionKind::Heraldry,
            MissionKind::DataCenterTurnIn,
            MissionKind::ResearchTurnIn,
        ];
        assert_eq!(MissionState::Offered.client_state_num(), 1);
        assert_eq!(MissionState::Accepted.client_state_num(), 2);
        assert_eq!(MissionState::Failed.client_state_num(), 3);
        assert_eq!(MissionState::Expired.client_state_num(), 3);
        assert_eq!(MissionState::Completed.client_state_num(), 4);
        assert_eq!(MissionState::Declined.client_state_num(), 5);
        assert_eq!(MissionState::Quit.client_state_num(), 6);
        assert_eq!(MissionState::parse("Offered"), Some(MissionState::Offered));
        assert_eq!(MissionState::parse("offered"), None);

        let mut store = MissionInstanceStore::new();
        for (idx, (&state, &kind)) in all_states.iter().zip(all_kinds.iter()).enumerate() {
            let iid = 200_000_100 - (idx as i64) * 3;
            let inst = sample_instance(iid, 90_000_001 + (idx as i64 % 2), 10_000 + idx as u32, state, kind);
            store.try_insert(inst).expect("insert sample");
        }
        store.assert_invariants().expect("valid store");

        let json = serde_json::to_string(&store).expect("serialize store");
        let decoded: MissionInstanceStore = serde_json::from_str(&json).expect("deserialize store");
        assert_eq!(decoded, store);
        decoded.assert_invariants().expect("decoded store invariants");

        // Ascending deterministic iteration order regardless of insertion order
        let ids: Vec<i64> = decoded.iter().map(|m| m.instance_id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted);

        // Secondary indexes lookup cleanly
        let enc = decoded
            .get_by_character_template(90_000_001, 10_000)
            .expect("char/template lookup");
        assert_eq!(enc.kind, MissionKind::Encounter);
        assert_eq!(
            decoded.get_by_npc_ball(enc.spawned_npc_ball_ids()[0]).map(|m| m.instance_id),
            Some(enc.instance_id)
        );
        assert_eq!(decoded.iter_by_character(90_000_001).count(), 4);
        assert_eq!(decoded.iter_by_character(90_000_002).count(), 3);
    }

    struct SplitMix64(u64);
    impl SplitMix64 {
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn next_range(&mut self, bound: usize) -> usize {
            (self.next_u64() as usize) % bound
        }
    }

    #[test]
    fn randomized_mission_store_ops_preserve_secondary_index_invariants() {
        let mut rng = SplitMix64(0x1496_5EED_CAFE_BABEu64);
        let mut store = MissionInstanceStore::new();
        let mut oracle: BTreeMap<MissionInstanceId, MissionInstance> = BTreeMap::new();
        let mut next_id = 200_000_001_i64;
        let mut next_ball = 910_000_001_i64;
        let states = [
            MissionState::Offered,
            MissionState::Accepted,
            MissionState::Completed,
            MissionState::Quit,
        ];

        for step in 0..800 {
            match rng.next_range(6) {
                0 | 1 => {
                    // Upsert / insert random encounter mission
                    let cid = 90_000_001 + rng.next_range(6) as i64;
                    let tid = 500 + rng.next_range(10) as u32;
                    let iid = next_id;
                    next_id += 1;
                    let b1 = next_ball;
                    let b2 = next_ball + 1;
                    next_ball += 2;
                    let state = states[rng.next_range(states.len())];
                    let mut inst = sample_instance(iid, cid, tid, state, MissionKind::Encounter);
                    if let Some(enc) = inst.encounter_mut() {
                        enc.spawned_npc_ball_ids = vec![b1, b2];
                        enc.npcs_remaining = 2;
                    }
                    let existing_key_id = oracle
                        .values()
                        .find(|m| m.character_id == cid && m.template_id == tid)
                        .map(|m| m.instance_id);
                    if let Some(old_id) = existing_key_id {
                        oracle.remove(&old_id);
                    }
                    oracle.insert(iid, inst.clone());
                    store
                        .upsert_by_character_template(inst)
                        .expect("upsert must succeed");
                }
                2 => {
                    // Update an existing instance (changing state, room, or ball list)
                    let ids: Vec<i64> = oracle.keys().copied().collect();
                    if let Some(&iid) = ids.get(rng.next_range(ids.len().max(1))) {
                        let new_state = states[rng.next_range(states.len())];
                        let new_room = (step % 5) as u32;
                        let new_ball = next_ball;
                        next_ball += 1;
                        let mutate_fn = |m: &mut MissionInstance| {
                            m.state = new_state;
                            m.current_room_id = new_room;
                            if let Some(enc) = m.encounter_mut() {
                                if enc.spawned_npc_ball_ids.len() > 1 {
                                    enc.spawned_npc_ball_ids.pop();
                                } else {
                                    enc.spawned_npc_ball_ids.push(new_ball);
                                }
                            }
                        };
                        mutate_fn(oracle.get_mut(&iid).unwrap());
                        store.update(iid, mutate_fn).expect("update must succeed");
                    }
                }
                3 => {
                    // Kill an NPC (either indexed or unrelated)
                    let all_balls: Vec<i64> = oracle
                        .values()
                        .flat_map(|m| m.spawned_npc_ball_ids().iter().copied())
                        .collect();
                    let (target_ball, should_hit) = if !all_balls.is_empty() && rng.next_range(2) == 0 {
                        (all_balls[rng.next_range(all_balls.len())], true)
                    } else {
                        (888_000_000 + rng.next_range(50) as i64, false)
                    };
                    let before_snap = store.clone();
                    let hit = store.on_npc_destroyed(30_000_142, target_ball);
                    if !should_hit {
                        assert!(!hit);
                        assert!(store.ptr_eq(&before_snap));
                    } else {
                        let oracle_hit = oracle.values_mut().any(|m| {
                            if m.state == MissionState::Accepted && m.solar_system_id == 30_000_142 {
                                if let Some(enc) = m.encounter_mut() {
                                    let before = enc.spawned_npc_ball_ids.len();
                                    enc.spawned_npc_ball_ids.retain(|&b| b != target_ball);
                                    if enc.spawned_npc_ball_ids.len() < before {
                                        enc.npcs_remaining = enc.npcs_remaining.saturating_sub(1);
                                        return true;
                                    }
                                }
                            }
                            false
                        });
                        assert_eq!(hit, oracle_hit);
                        if !oracle_hit {
                            assert!(store.ptr_eq(&before_snap));
                        }
                    }
                }
                4 => {
                    // Remove by instance id
                    let ids: Vec<i64> = oracle.keys().copied().collect();
                    if let Some(&iid) = ids.get(rng.next_range(ids.len().max(1))) {
                        assert_eq!(store.remove(iid), oracle.remove(&iid));
                    }
                }
                _ => {
                    // Remove by (character_id, template_id)
                    let cid = 90_000_001 + rng.next_range(6) as i64;
                    let tid = 500 + rng.next_range(10) as u32;
                    let found_id = oracle
                        .values()
                        .find(|m| m.character_id == cid && m.template_id == tid)
                        .map(|m| m.instance_id);
                    let expected = found_id.and_then(|id| oracle.remove(&id));
                    assert_eq!(store.remove_by_character_template(cid, tid), expected);
                }
            }

            store.assert_invariants().expect("invariants must hold every step");
            let store_vec: Vec<&MissionInstance> = store.iter().collect();
            let oracle_vec: Vec<&MissionInstance> = oracle.values().collect();
            assert_eq!(store_vec, oracle_vec);
            for cid in 90_000_001..=90_000_006 {
                let by_char_store: Vec<&MissionInstance> = store.iter_by_character(cid).collect();
                let by_char_oracle: Vec<&MissionInstance> =
                    oracle.values().filter(|m| m.character_id == cid).collect();
                assert_eq!(by_char_store, by_char_oracle);
            }
        }
    }
}
