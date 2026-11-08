//! `quasar_career_goals` — V24.01 Quasar AIR Career Program Goals Service (`feat:quasar.career`, `Q-4`).
//!
//! Implements live backend semantics for the EVE Online V24 AIR Career Program:
//! - 4 Career paths: Enforcer, Explorer, Industrialist, Soldier of Fortune.
//! - Authentic AIR Career Program goal definitions, progress tracking, career points, milestone rewards.
//! - Request handlers for `eve_public.career.goal.api`, `eve.career.goal.api`, and `eve_public.app.eveonline.career.goal`.
//! - Live notice publication (`ProgressedNotice`, `CompletedNotice`).
//! - Live wallet/skill-point payouts in `WorldRepository`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
use eve_db::{IskCents, WorldRepository};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, DynamicValue, GoogleAny, WireMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

/// Authentic V24 `res:/staticdata/careerpaths.fsdbinary` base localization message IDs `(nameID, descriptionID)`:
/// - `1` (`CAREER_EXPLORATION`, FSD key 4 `"Explorer"`): `(582_516, 582_517)`
/// - `2` (`CAREER_INDUSTRIALIST`, FSD key 5 `"Industrialist"`): `(582_518, 582_519)`
/// - `3` (`CAREER_ENFORCER`, FSD key 6 `"Enforcer"`): `(582_520, 582_521)`
/// - `4` (`CAREER_SOLDIER_OF_FORTUNE`, FSD key 7 `"Soldier of Fortune"`): `(582_522, 582_523)`
pub const CAREER_PATH_MESSAGE_IDS: [(u32, u32, u32); 4] = [
    (1, 582_516, 582_517),
    (2, 582_518, 582_519),
    (3, 582_520, 582_521),
    (4, 582_522, 582_523),
];

/// DERIVED: AIR Career Program goal definition. Career path IDs (`1..=4`), activity groups, targets,
/// career points, and ISK/SP rewards are synthesized from SDE (`careerpaths.fsdbinary`,
/// `contributionmethods.fsdbinary`) and EVE University Wiki AIR Career Program tables; per-goal
/// `name_message_id` (`582531..582689`, `665435..665481`), `description_message_id` (`696260..696439`),
/// and `threat` are DERIVED offsets from client UI base localization IDs because individual goal
/// definitions are served dynamically by live Quasar.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CareerGoalDef {
    pub uuid: [u8; 16],
    pub career_id: u32, // 1: Explorer, 2: Industrialist, 3: Enforcer, 4: Soldier of Fortune (Attributes.Career)
    #[serde(default)]
    pub activity_name: String,
    #[serde(default)]
    pub activity_kind: String,
    pub name: String,
    pub description: String,
    pub target: u64,
    pub career_points: u32,
    pub isk_reward: i64,
    pub sp_reward: f64,
    #[serde(default)]
    pub name_message_id: u32,
    #[serde(default)]
    pub description_message_id: u32,
    #[serde(default)]
    pub threat: u32,
    #[serde(default)]
    pub duration: Option<u32>,
    #[serde(default)]
    pub aura_text_id: Option<u32>,
    #[serde(default)]
    pub agency_link_text_id: Option<u32>,
    #[serde(default)]
    pub agency_screen: Option<String>,
    #[serde(default)]
    pub order: Option<u32>,
    #[serde(default)]
    pub _provenance: Option<String>,
}

impl CareerGoalDef {
    /// DERIVED: Returns the localization message ID for `Attributes.name.sequential`, falling back
    /// to a derived offset (`base + uuid[15]`) from the `careerpaths.fsdbinary` career path base ID.
    pub fn effective_name_message_id(&self) -> u32 {
        if self.name_message_id > 0 {
            return self.name_message_id;
        }
        let base = CAREER_PATH_MESSAGE_IDS
            .iter()
            .find(|(cid, _, _)| *cid == self.career_id)
            .map(|(_, n, _)| *n)
            .unwrap_or(582_520);
        base + (self.uuid[15] as u32)
    }

    /// DERIVED: Returns the localization message ID for `Attributes.description.sequential`, falling
    /// back to a derived offset (`base + 1_000 + uuid[15]`) from the `careerpaths.fsdbinary` base ID.
    pub fn effective_description_message_id(&self) -> u32 {
        if self.description_message_id > 0 {
            return self.description_message_id;
        }
        let base = CAREER_PATH_MESSAGE_IDS
            .iter()
            .find(|(cid, _, _)| *cid == self.career_id)
            .map(|(_, _, d)| *d)
            .unwrap_or(582_521);
        base + 1_000 + (self.uuid[15] as u32)
    }

    /// Returns the canonical V24 `CAREER_PATH_GROUPS` activity group annotation key (`ui_annotations["group"]`).
    pub fn effective_group(&self) -> &'static str {
        let norm: String = self
            .activity_name
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        let kind = self.activity_kind.to_ascii_lowercase();
        match self.career_id.clamp(1, 4) {
            1 => match (norm.as_str(), kind.as_str()) {
                ("scanning", _) | (_, "exploration") => "Scanning",
                ("hacking", _) | (_, "hacking") => "Hacking",
                ("advancedhacking", _) | (_, "advanced_hacking") => "AdvancedHacking",
                ("wormholes", _) | (_, "wormholes") => "Wormholes",
                ("gassites", _) | (_, "gas_sites") => "GasSites",
                ("combatsites", _) | (_, "combat_sites") => "CombatSites",
                ("navigation", _) | (_, "travel") => "Navigation",
                ("advancednavigation", _) | (_, "advanced_travel") => "AdvancedNavigation",
                ("projectdiscovery", _) | (_, "project_discovery") => "ProjectDiscovery",
                _ => "CareerAgentMissions",
            },
            2 => match (norm.as_str(), kind.as_str()) {
                ("market", _) | (_, "market") => "Market",
                ("salvage", _) | (_, "salvage") => "Salvage",
                ("resourceharvesting", _) | (_, "mining") => "ResourceHarvesting",
                ("refining", _) | (_, "refining") => "Refining",
                ("manufacturing", _) | (_, "manufacturing") => "Manufacturing",
                ("distributionagent", _) | (_, "distribution_missions") => "DistributionAgent",
                ("miningagent", _) | (_, "mining_missions") => "MiningAgent",
                ("research", _) | (_, "research") => "Research",
                _ => "IndustrialistCareerAgentsMissions",
            },
            3 => match (norm.as_str(), kind.as_str()) {
                ("securityagent", _) | (_, "combat_missions") => "SecurityAgent",
                ("abyssaldeadspace", _) | (_, "abyssal") => "AbyssalDeadspace",
                ("combat", _) | (_, "combat") => "Combat",
                ("bounties", _) | (_, "bounties") => "Bounties",
                ("combatsites", _) | (_, "combat_sites") => "CombatSites",
                ("epicarc", _) | (_, "epic_arc") => "EpicArc",
                ("market", _) | (_, "market") => "Market",
                ("standings", _) | (_, "standings") => "Standings",
                ("loyaltypoints", _) | (_, "loyalty_points") => "LoyaltyPoints",
                _ => "CareerAgentMissions",
            },
            _ => match (norm.as_str(), kind.as_str()) {
                ("social", _) | (_, "social") => "Social",
                ("electronicwarfare", _) | (_, "ewar") => "ElectronicWarfare",
                ("combat", _) | (_, "combat") => "Combat",
                ("pvp", _) | (_, "pvp") => "PvP",
                ("capacitorwarfare", _) | (_, "cap_warfare") => "CapacitorWarfare",
                ("factionwarfare", _) | (_, "faction_warfare") => "FactionWarfare",
                ("support", _) | (_, "remote_repair") => "Support",
                ("destruction", _) | (_, "destruction") => "Destruction",
                ("duels", _) | (_, "duels") => "Duels",
                _ => "CareerAgentMissions",
            },
        }
    }

    /// Returns the goal threat level (`1..=3` for `Attributes.threat`).
    pub fn effective_threat(&self) -> u32 {
        if self.threat > 0 {
            self.threat.clamp(1, 3)
        } else if self.career_points <= 15 {
            1
        } else if self.career_points <= 25 {
            2
        } else {
            3
        }
    }

    /// Returns the estimated duration in minutes (`ui_annotations["duration"]`).
    pub fn effective_duration(&self) -> u32 {
        self.duration
            .filter(|&d| d > 0)
            .unwrap_or_else(|| (self.career_points * 2).max(5))
    }

    /// Returns the V24.01 Aura hint localization message ID (`ui_annotations["auraTextID"]`),
    /// falling back to `effective_description_message_id()` when unspecified.
    pub fn effective_aura_text_id(&self) -> u32 {
        self.aura_text_id
            .filter(|&id| id > 0)
            .unwrap_or_else(|| self.effective_description_message_id())
    }

    /// Returns the display sort order within its activity group (`ui_annotations["order"]`).
    pub fn effective_order(&self) -> u32 {
        self.order
            .filter(|&o| o > 0)
            .unwrap_or_else(|| (self.uuid[15] as u32).max(1))
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CareerMilestoneDef {
    pub milestone_id: u32,
    pub career_id: Option<u32>, // None = overall AIR milestone
    pub required_points: u32,
    pub sp_reward: f64,
    pub description: String,
}

/// DERIVED: Server-internal career milestone identifier convention for ClaimRewardRequest multiplexing.
///
/// In the Quasar Career Goal protocol (`eve_public.career.goal.api.requests.ClaimRewardRequest` /
/// `eve.career.goal.api.requests.ClaimRewardRequest`), reward claims are keyed by a 16-byte UUID
/// (`Identifier { uuid: bytes }`). Authentic AIR Career Program milestone rewards (4 career graduations
/// with IDs 1..=4 and 4 overall CP milestones with IDs 101..=104) are defined with integer milestone IDs.
///
/// To support claiming career milestone rewards through the standard Quasar `ClaimRewardRequest` endpoint
/// without inventing a separate proprietary RPC, the server maps 4-byte `milestone_id`s into a 16-byte UUID
/// prefixed with ASCII `"CM"` (`[0x43, 0x4D]`) and the 4-byte big-endian milestone ID at offset 12..16.
/// When `ClaimRewardRequest` receives a target UUID with this `"CM"` prefix, it routes to `claim_milestone_reward`.
pub fn career_milestone_uuid(milestone_id: u32) -> [u8; 16] {
    let mut u = [0u8; 16];
    u[0] = 0x43; // 'C'
    u[1] = 0x4D; // 'M'
    u[12..16].copy_from_slice(&milestone_id.to_be_bytes());
    u
}

/// DERIVED: Authentic AIR Career Program milestone definitions (4 Career graduations at 750 CP + 4 overall CP milestones).
/// Per ~/ai/eve-wiki/articles/AIR_Career_Program.md:20-60, graduation rewards 100k SP per career (4 x 100,000 = 400,000 SP),
/// and overall milestones reward 50k (750 CP), 75k (1500 CP), 100k (2250 CP), 125k (3000 CP) for 350,000 SP,
/// reaching exactly 750,000 SP across the AIR Career Program.
pub fn default_career_milestones() -> Vec<CareerMilestoneDef> {
    vec![
        // Career graduation milestones (750 CP per career, 75% completion)
        CareerMilestoneDef {
            milestone_id: 1,
            career_id: Some(1), // Explorer
            required_points: 750,
            sp_reward: 100_000.0,
            description: "Explorer Career Graduation".into(),
        },
        CareerMilestoneDef {
            milestone_id: 2,
            career_id: Some(2), // Industrialist
            required_points: 750,
            sp_reward: 100_000.0,
            description: "Industrialist Career Graduation".into(),
        },
        CareerMilestoneDef {
            milestone_id: 3,
            career_id: Some(3), // Enforcer
            required_points: 750,
            sp_reward: 100_000.0,
            description: "Enforcer Career Graduation".into(),
        },
        CareerMilestoneDef {
            milestone_id: 4,
            career_id: Some(4), // Soldier of Fortune
            required_points: 750,
            sp_reward: 100_000.0,
            description: "Soldier of Fortune Career Graduation".into(),
        },
        // Overall Career Program milestones
        CareerMilestoneDef {
            milestone_id: 101,
            career_id: None,
            required_points: 750,
            sp_reward: 50_000.0,
            description: "AIR Career Program 750 CP Milestone".into(),
        },
        CareerMilestoneDef {
            milestone_id: 102,
            career_id: None,
            required_points: 1500,
            sp_reward: 75_000.0,
            description: "AIR Career Program 1500 CP Milestone".into(),
        },
        CareerMilestoneDef {
            milestone_id: 103,
            career_id: None,
            required_points: 2250,
            sp_reward: 100_000.0,
            description: "AIR Career Program 2250 CP Milestone".into(),
        },
        CareerMilestoneDef {
            milestone_id: 104,
            career_id: None,
            required_points: 3000,
            sp_reward: 125_000.0,
            description: "AIR Career Program 3000 CP Milestone".into(),
        },
    ]
}

/// DERIVED: Authentic AIR Career Program goal definitions loaded from static definitions.
/// Synthesized 16-byte UUIDs provide deterministic identifiers matching client wire schemas,
/// and individual goal sp_reward values are 0.0 per ~/ai/eve-wiki/articles/AIR_Career_Program.md
/// because all 750,000 Skill Points are awarded exclusively via career graduation (4 x 100k SP)
/// and overall career point milestones (50k, 75k, 100k, 125k SP).
pub fn default_career_goal_definitions() -> Vec<CareerGoalDef> {
    const RAW_JSON: &str = include_str!("../../../../data/air_career_goals.json");
    serde_json::from_str(RAW_JSON).unwrap_or_else(|_| Vec::new())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CharacterCareerGoal {
    pub uuid: [u8; 16],
    pub def: CareerGoalDef,
    pub progress: u64,
    pub completed: bool,
    pub claimed: bool,
    pub completed_time: Option<u64>,
}

#[derive(Default)]
pub struct CareerGoalManager {
    pub definitions: RwLock<Vec<CareerGoalDef>>,
    pub character_career_goals: RwLock<HashMap<u32, Vec<CharacterCareerGoal>>>,
    pub claimed_milestones: RwLock<HashMap<u32, HashSet<u32>>>,
    pub milestones: Vec<CareerMilestoneDef>,
    db: Option<Arc<WorldRepository>>,
}

fn ensure_character_career_goals_with_defs(
    goals: &mut Vec<CharacterCareerGoal>,
    defs: &[CareerGoalDef],
) {
    goals.retain(|g| defs.iter().any(|d| d.uuid == g.uuid));
    for g in goals.iter_mut() {
        if g.def.activity_kind.is_empty() {
            if let Some(def) = defs.iter().find(|d| d.uuid == g.uuid) {
                g.def.activity_kind = def.activity_kind.clone();
            }
        }
    }
    for def in defs {
        if !goals.iter().any(|g| g.uuid == def.uuid) {
            goals.push(CharacterCareerGoal {
                uuid: def.uuid,
                def: def.clone(),
                progress: 0,
                completed: false,
                claimed: false,
                completed_time: None,
            });
        }
    }
}

impl CareerGoalManager {
    pub fn new(db: Option<Arc<WorldRepository>>) -> Self {
        let defs = db
            .as_ref()
            .and_then(|d| {
                d.with_snapshot(|s| {
                    s.quasar_state_blobs
                        .get("quasar_career_goal_defs")
                        .and_then(|json| serde_json::from_str::<Vec<CareerGoalDef>>(json).ok())
                })
            })
            .unwrap_or_else(default_career_goal_definitions);
        let mut loaded = db
            .as_ref()
            .map(|d| {
                d.with_snapshot(|s| {
                    s.quasar_state_blobs
                        .get("quasar_career_goals")
                        .and_then(|json| {
                            serde_json::from_str::<HashMap<u32, Vec<CharacterCareerGoal>>>(json)
                                .ok()
                        })
                        .unwrap_or_default()
                })
            })
            .unwrap_or_default();
        for goals in loaded.values_mut() {
            ensure_character_career_goals_with_defs(goals, &defs);
        }
        let loaded_milestones = db
            .as_ref()
            .map(|d| {
                d.with_snapshot(|s| {
                    let mut out = s
                        .quasar_state_blobs
                        .get("quasar_career_milestones")
                        .and_then(|json| {
                            serde_json::from_str::<HashMap<u32, HashSet<u32>>>(json).ok()
                        })
                        .unwrap_or_default();
                    let valid_mids: HashSet<u32> = default_career_milestones()
                        .into_iter()
                        .map(|m| m.milestone_id)
                        .collect();
                    for (&cid, npe) in &s.npe_progress {
                        if let Ok(u32_cid) = u32::try_from(cid) {
                            for &gid in &npe.claimed_goals {
                                if valid_mids.contains(&gid) {
                                    out.entry(u32_cid).or_default().insert(gid);
                                }
                            }
                        }
                    }
                    out
                })
            })
            .unwrap_or_default();

        Self {
            definitions: RwLock::new(defs),
            character_career_goals: RwLock::new(loaded),
            claimed_milestones: RwLock::new(loaded_milestones),
            milestones: default_career_milestones(),
            db,
        }
    }

    pub fn load_definitions(&self) -> Vec<CareerGoalDef> {
        let defs = self.definitions.read().unwrap_or_else(|e| e.into_inner());
        if defs.is_empty() {
            default_career_goal_definitions()
        } else {
            defs.clone()
        }
    }

    pub fn configure_definition(&self, def: CareerGoalDef) {
        let defs = {
            let mut d = self.definitions.write().unwrap_or_else(|e| e.into_inner());
            if d.is_empty() {
                *d = default_career_goal_definitions();
            }
            if let Some(existing) = d.iter_mut().find(|x| x.uuid == def.uuid) {
                *existing = def;
            } else {
                d.push(def);
            }
            d.clone()
        };
        {
            let mut map = self
                .character_career_goals
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for goals in map.values_mut() {
                ensure_character_career_goals_with_defs(goals, &defs);
            }
        }
        self.save_to_db();
    }

    pub fn save_to_db(&self) {
        if let Some(ref db) = self.db {
            if db.is_write_locked_by_current_thread() {
                return;
            }
            let defs = self.definitions.read().unwrap_or_else(|e| e.into_inner());
            let map = self.character_career_goals.read().unwrap_or_else(|e| e.into_inner());
            let cm = self.claimed_milestones.read().unwrap_or_else(|e| e.into_inner());
            let defs_json = if defs.is_empty() {
                None
            } else {
                serde_json::to_string(&*defs).ok()
            };
            let goals_json = serde_json::to_string(&*map).ok();
            let milestones_json = serde_json::to_string(&*cm).ok();
            let _ = db.try_with_mut_snapshot(|snap| {
                if let Some(d_json) = defs_json {
                    snap.quasar_state_blobs
                        .insert("quasar_career_goal_defs".into(), d_json);
                }
                if let Some(json) = goals_json {
                    snap.quasar_state_blobs.insert("quasar_career_goals".into(), json);
                }
                if let Some(m_json) = milestones_json {
                    snap.quasar_state_blobs.insert("quasar_career_milestones".into(), m_json);
                }
                Ok::<(), String>(())
            });
        }
    }

    pub fn rehydrate_from_db(&self) {
        let Some(ref db) = self.db else { return };
        let (defs, mut loaded, loaded_milestones) = db.with_snapshot(|s| {
            let defs = s
                .quasar_state_blobs
                .get("quasar_career_goal_defs")
                .and_then(|json| serde_json::from_str::<Vec<CareerGoalDef>>(json).ok())
                .unwrap_or_else(default_career_goal_definitions);
            let loaded = s
                .quasar_state_blobs
                .get("quasar_career_goals")
                .and_then(|json| {
                    serde_json::from_str::<HashMap<u32, Vec<CharacterCareerGoal>>>(json).ok()
                })
                .unwrap_or_default();
            let mut out = s
                .quasar_state_blobs
                .get("quasar_career_milestones")
                .and_then(|json| serde_json::from_str::<HashMap<u32, HashSet<u32>>>(json).ok())
                .unwrap_or_default();
            let valid_mids: HashSet<u32> = default_career_milestones()
                .into_iter()
                .map(|m| m.milestone_id)
                .collect();
            for (&cid, npe) in &s.npe_progress {
                if let Ok(u32_cid) = u32::try_from(cid) {
                    for &gid in &npe.claimed_goals {
                        if valid_mids.contains(&gid) {
                            out.entry(u32_cid).or_default().insert(gid);
                        }
                    }
                }
            }
            (defs, loaded, out)
        });
        for goals in loaded.values_mut() {
            ensure_character_career_goals_with_defs(goals, &defs);
        }
        *self.definitions.write().unwrap_or_else(|e| e.into_inner()) = defs;
        *self
            .character_career_goals
            .write()
            .unwrap_or_else(|e| e.into_inner()) = loaded;
        *self
            .claimed_milestones
            .write()
            .unwrap_or_else(|e| e.into_inner()) = loaded_milestones;
    }

    fn ensure_goals_for_list(&self, goals: &mut Vec<CharacterCareerGoal>) {
        let defs = self.load_definitions();
        ensure_character_career_goals_with_defs(goals, &defs);
    }

    pub fn get_or_init_goals(&self, character_id: u32) -> Vec<CharacterCareerGoal> {
        let mut map = self
            .character_career_goals
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let goals = map.entry(character_id).or_default();
        self.ensure_goals_for_list(goals);
        goals.clone()
    }

    pub fn career_points_for_character(&self, character_id: u32, career_id: Option<u32>) -> u32 {
        let map = self
            .character_career_goals
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let Some(goals) = map.get(&character_id) else {
            return 0;
        };
        goals
            .iter()
            .filter(|g| g.completed && career_id.map_or(true, |cid| g.def.career_id == cid))
            .map(|g| g.def.career_points)
            .sum()
    }

    pub fn on_gameplay_event(
        &self,
        char_id: u32,
        activity_kind: &str,
        delta: u64,
        gateway: &PublicGatewayRouter,
    ) {
        let now_unix = self
            .db
            .as_ref()
            .map(|db| db.clock().now_unix_secs())
            .unwrap_or(0);
        let mut progressed = Vec::new();
        {
            let mut map = self
                .character_career_goals
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let goals = map.entry(char_id).or_default();
            self.ensure_goals_for_list(goals);

            for g in goals.iter_mut() {
                if g.def.activity_kind == activity_kind && !g.completed {
                    let prev = g.completed;
                    g.progress = g.progress.saturating_add(delta);
                    if g.progress >= g.def.target {
                        g.completed = true;
                        g.completed_time = Some(now_unix);
                    }
                    let just_completed = !prev && g.completed;
                    progressed.push((g.uuid, g.progress, just_completed));
                }
            }
        }
        self.save_to_db();

        for (uuid, new_progress, just_completed) in progressed {
            let mut id_bytes = Vec::new();
            encode_career_goal_identifier(&uuid, &mut id_bytes);
            let mut not_bytes = Vec::new();
            encode_bytes_field(1, &id_bytes, &mut not_bytes);
            encode_varint_field(2, new_progress, &mut not_bytes);

            let _ = gateway.publish_notice(
                crate::quasar_gateway::NoticeScope::Character(char_id),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.ProgressedNotice"),
                    value: not_bytes,
                },
                now_unix,
            );

            if just_completed {
                let mut comp_bytes = Vec::new();
                encode_bytes_field(1, &id_bytes, &mut comp_bytes);
                let _ = gateway.publish_notice(
                    crate::quasar_gateway::NoticeScope::Character(char_id),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.CompletedNotice"),
                        value: comp_bytes,
                    },
                    now_unix,
                );
            }
        }
    }

    pub fn add_progress(
        &self,
        character_id: u32,
        goal_uuid: &[u8; 16],
        progress_delta: u64,
        now_unix: u64,
    ) -> Option<(CharacterCareerGoal, bool)> {
        let mut map = self
            .character_career_goals
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let goals = map.entry(character_id).or_default();
        self.ensure_goals_for_list(goals);

        for g in goals.iter_mut() {
            if &g.uuid == goal_uuid {
                let prev_completed = g.completed;
                g.progress = g.progress.saturating_add(progress_delta);
                if g.progress >= g.def.target && !g.completed {
                    g.completed = true;
                    g.completed_time = Some(now_unix);
                }
                let just_completed = !prev_completed && g.completed;
                return Some((g.clone(), just_completed));
            }
        }
        None
    }

    pub fn mark_completed(
        &self,
        character_id: u32,
        goal_uuid: &[u8; 16],
        now_unix: u64,
    ) -> Result<CharacterCareerGoal, (u32, String)> {
        let mut map = self
            .character_career_goals
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let goals = map.entry(character_id).or_default();
        self.ensure_goals_for_list(goals);

        for g in goals.iter_mut() {
            if &g.uuid == goal_uuid {
                g.progress = g.def.target;
                g.completed = true;
                g.completed_time = Some(now_unix);
                return Ok(g.clone());
            }
        }
        Err((STATUS_NOT_FOUND, "Career goal not found".into()))
    }

    pub fn claim_reward(
        &self,
        character_id: u32,
        goal_uuid: &[u8; 16],
        db: &WorldRepository,
    ) -> Result<CharacterCareerGoal, (u32, String)> {
        let (res_goal, isk, sp) = {
            let mut map = self
                .character_career_goals
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let Some(goals) = map.get_mut(&character_id) else {
                return Err((STATUS_NOT_FOUND, "No career goals for character".into()));
            };

            let mut found = None;
            for g in goals.iter_mut() {
                if &g.uuid == goal_uuid {
                    if !g.completed {
                        return Err((STATUS_FORBIDDEN, "Career goal not yet completed".into()));
                    }
                    if g.claimed {
                        return Err((STATUS_FORBIDDEN, "Career goal already claimed".into()));
                    }
                    g.claimed = true;
                    found = Some((g.clone(), g.def.isk_reward, g.def.sp_reward));
                    break;
                }
            }
            match found {
                Some(f) => f,
                None => return Err((STATUS_NOT_FOUND, "Career goal not found".into())),
            }
        };

        if isk > 0 || sp > 0.0 {
            let credit_res = db.try_with_mut_snapshot(|snap| {
                let Some(ch) = snap.characters.get_mut(&(character_id as i64)) else {
                    return Err("Character not found".to_string());
                };
                let isk_cents = IskCents::from_isk(isk);
                let Some(new_bal) = ch.balance.checked_add(isk_cents) else {
                    return Err("Balance overflow".to_string());
                };
                ch.balance = new_bal;
                ch.free_skill_points += sp;
                if isk > 0 {
                    let now_unix = db.clock().now_unix_secs();
                    snap.append_journal_entry(eve_db::ledger::JournalEntryInput {
                        owner_id: character_id as i64,
                        entry_type: "CAREER_PROGRAM_REWARD",
                        amount: isk_cents,
                        balance_after: new_bal,
                        reason: format!("AIR Career Program goal reward ({})", res_goal.def.name),
                        meta: eve_db::ledger::JournalMeta {
                            ref_type_id: 170, // CAREER_PROGRAM_REWARD
                            first_party_id: 1000125, // CONCORD
                            second_party_id: character_id as i64,
                            account_key: eve_db::ledger::ACCOUNTING_KEY_CASH,
                            date_unix: now_unix,
                            ..Default::default()
                        },
                    });
                }
                Ok(())
            });
            if credit_res.is_err() {
                let mut map = self
                    .character_career_goals
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(goals) = map.get_mut(&character_id) {
                    for g in goals.iter_mut() {
                        if &g.uuid == goal_uuid {
                            g.claimed = false;
                            break;
                        }
                    }
                }
                return Err((STATUS_BAD_REQUEST, "Unable to credit career goal reward".into()));
            }
        }

        Ok(res_goal)
    }

    pub fn get_character_milestones(&self, character_id: u32) -> Vec<(CareerMilestoneDef, bool, bool)> {
        let mut claimed_set = self
            .claimed_milestones
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&character_id)
            .cloned()
            .unwrap_or_default();
        if let Some(ref db) = self.db {
            db.with_snapshot(|snap| {
                if let Some(npe) = snap.npe_progress.get(&(character_id as i64)) {
                    claimed_set.extend(&npe.claimed_goals);
                }
            });
        }

        let total_cp = self.career_points_for_character(character_id, None);

        self.milestones
            .iter()
            .map(|m| {
                let cp = match m.career_id {
                    Some(cid) => self.career_points_for_character(character_id, Some(cid)),
                    None => total_cp,
                };
                let reached = cp >= m.required_points;
                let claimed = claimed_set.contains(&m.milestone_id);
                (m.clone(), reached, claimed)
            })
            .collect()
    }

    pub fn is_milestone_claimed(
        &self,
        character_id: u32,
        milestone_id: u32,
        snap: &eve_db::WorldStateSnapshot,
    ) -> bool {
        let in_mem = self
            .claimed_milestones
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&character_id)
            .is_some_and(|s| s.contains(&milestone_id));
        let in_npe = snap
            .npe_progress
            .get(&(character_id as i64))
            .is_some_and(|n| n.claimed_goals.contains(&milestone_id));
        let in_blob = snap
            .quasar_state_blobs
            .get("quasar_career_milestones")
            .and_then(|j| serde_json::from_str::<HashMap<u32, HashSet<u32>>>(j).ok())
            .and_then(|m| m.get(&character_id).map(|s| s.contains(&milestone_id)))
            .unwrap_or(false);
        in_mem || in_npe || in_blob
    }

    pub fn mark_milestone_claimed_in_snapshot(
        &self,
        character_id: u32,
        milestone_id: u32,
        snap: &mut eve_db::WorldStateSnapshot,
    ) {
        let json = {
            let mut cm = self
                .claimed_milestones
                .write()
                .unwrap_or_else(|e| e.into_inner());
            cm.entry(character_id).or_default().insert(milestone_id);
            serde_json::to_string(&*cm).ok()
        };
        if let Some(j) = json {
            snap.quasar_state_blobs
                .insert("quasar_career_milestones".into(), j);
        }
        let npe = snap
            .npe_progress
            .entry(character_id as i64)
            .or_insert_with(|| eve_db::NpeProgressRecord {
                character_id: character_id as i64,
                node_graph_id: 473,
                current_node_id: 1,
                completed: false,
                career_goal_progress: Default::default(),
                claimed_goals: Default::default(),
            });
        npe.claimed_goals.insert(milestone_id);
    }

    /// DERIVED: Claims an authentic AIR Career Program milestone reward (graduation 1..=4 or overall CP milestone 101..=104).
    pub fn claim_milestone_reward(
        &self,
        character_id: u32,
        milestone_id: u32,
        db: &WorldRepository,
    ) -> Result<CareerMilestoneDef, (u32, String)> {
        let milestone = self
            .milestones
            .iter()
            .find(|m| m.milestone_id == milestone_id)
            .cloned()
            .ok_or_else(|| (STATUS_NOT_FOUND, "Career milestone not found".into()))?;

        let cp = match milestone.career_id {
            Some(cid) => self.career_points_for_character(character_id, Some(cid)),
            None => self.career_points_for_character(character_id, None),
        };

        if cp < milestone.required_points {
            return Err((STATUS_FORBIDDEN, format!("Career points {cp} below milestone threshold {}", milestone.required_points)));
        }

        db.try_with_mut_snapshot(|snap| {
            if self.is_milestone_claimed(character_id, milestone_id, snap) {
                return Err((STATUS_FORBIDDEN, "Career milestone reward already claimed".into()));
            }
            let Some(ch) = snap.characters.get_mut(&(character_id as i64)) else {
                return Err((STATUS_BAD_REQUEST, "Unable to credit career milestone reward".into()));
            };
            ch.free_skill_points += milestone.sp_reward;
            self.mark_milestone_claimed_in_snapshot(character_id, milestone_id, snap);
            Ok(())
        })?;
        self.save_to_db();

        Ok(milestone)
    }
}

pub use super::quasar_career_goals_wire::{
    encode_career_goal_definition_wire, encode_career_goal_identifier, encode_career_goal_wire,
    encode_career_milestone_definition_wire, encode_career_milestone_goal_wire,
    encode_internal_career_goal_definition_wire, encode_internal_career_milestone_definition_wire,
    extract_target_character, extract_uuid,
};

// ============================================================================
// Registration
// ============================================================================

pub fn register_career_goal_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    manager: Arc<CareerGoalManager>,
) {
    // 1. eve_public.career.goal.api.GetAllRequest -> GetAllResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.career.goal.api.GetAllRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let goals = mgr.get_or_init_goals(cid);
            let mut out = Vec::new();
            for g in &goals {
                let mut gw = Vec::new();
                encode_career_goal_wire(g, &mut gw);
                encode_bytes_field(1, &gw, &mut out);
            }
            for (ms, reached, claimed) in mgr.get_character_milestones(cid) {
                let cp = mgr.career_points_for_character(cid, ms.career_id);
                let mut mw = Vec::new();
                encode_career_milestone_goal_wire(&ms, cp, reached, claimed, &mut mw);
                encode_bytes_field(1, &mw, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.GetAllResponse"),
                value: out,
            })
        }),
    );

    // 2. eve_public.career.goal.api.GetDefinitionsRequest (and .requests. alias) -> GetDefinitionsResponse
    for req_name in [
        "eve_public.career.goal.api.GetDefinitionsRequest",
        "eve_public.career.goal.api.requests.GetDefinitionsRequest",
    ] {
        let mgr_defs = Arc::clone(&manager);
        router.register_handler(
            req_name,
            Arc::new(move |_ctx, _any, _dyn| {
                let defs = mgr_defs.load_definitions();
                let mut out = Vec::new();
                for def in &defs {
                    let mut gw = Vec::new();
                    encode_career_goal_definition_wire(def, &mut gw);
                    encode_bytes_field(1, &gw, &mut out);
                }
                for ms in &mgr_defs.milestones {
                    let mut mw = Vec::new();
                    encode_career_milestone_definition_wire(ms, &mut mw);
                    encode_bytes_field(1, &mw, &mut out);
                }
                Ok(GoogleAny {
                    type_url: format!(
                        "{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.GetDefinitionsResponse"
                    ),
                    value: out,
                })
            }),
        );
    }

    // 3. eve_public.career.goal.api.ClaimRewardRequest -> ClaimRewardResponse
    let mgr = Arc::clone(&manager);
    let db_claim = Arc::clone(&db);
    router.register_handler(
        "eve_public.career.goal.api.ClaimRewardRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            if target_uuid[0] == 0x43 && target_uuid[1] == 0x4D {
                let mut mid_bytes = [0u8; 4];
                mid_bytes.copy_from_slice(&target_uuid[12..16]);
                let mid = u32::from_be_bytes(mid_bytes);
                let _ = mgr.claim_milestone_reward(cid, mid, &db_claim)?;
            } else {
                let _ = mgr.claim_reward(cid, &target_uuid, &db_claim)?;
                mgr.save_to_db();
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.ClaimRewardResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 4. eve_public.career.goal.api.admin.ProgressRequest -> ProgressResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.career.goal.api.admin.ProgressRequest",
        Arc::new(move |ctx, any, dyn_msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let Some(target_char) = extract_target_character(dyn_msg, any, 3, true)
                .or_else(|| extract_target_character(dyn_msg, any, 2, false))
                .or_else(|| ctx.effective_character_id())
            else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let delta = dyn_msg
                .get(2)
                .and_then(|v| match v {
                    DynamicValue::Uint64(u) => Some(*u),
                    DynamicValue::Int64(i) => Some(*i as u64),
                    DynamicValue::Uint32(u) => Some(*u as u64),
                    DynamicValue::Int32(i) => Some(*i as u64),
                    _ => None,
                })
                .or_else(|| {
                    WireMessage::decode(&any.value).ok().and_then(|wm| {
                        wm.fields
                            .iter()
                            .find(|f| f.field_number == 3)
                            .and_then(|f| f.value.as_varint())
                            .filter(|&v| v < 1_000_000)
                    })
                })
                .unwrap_or(1);
            if delta == 0 {
                return Err((STATUS_BAD_REQUEST, "Progress must be positive".into()));
            }
            let Some((goal, just_completed)) =
                mgr.add_progress(target_char, &target_uuid, delta, ctx.now_unix())
            else {
                return Err((STATUS_NOT_FOUND, "Career goal not found".into()));
            };
            mgr.save_to_db();

            let mut not_bytes = Vec::new();
            let mut id_bytes = Vec::new();
            encode_career_goal_identifier(&target_uuid, &mut id_bytes);
            encode_bytes_field(1, &id_bytes, &mut not_bytes);
            encode_varint_field(2, goal.progress, &mut not_bytes);
            super::quasar_social_chat::ctx_queue_notice(
                crate::quasar_gateway::NoticeScope::Character(target_char),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.ProgressedNotice"),
                    value: not_bytes,
                },
                ctx.now_unix(),
            );

            if just_completed {
                let mut comp_bytes = Vec::new();
                encode_bytes_field(1, &id_bytes, &mut comp_bytes);
                super::quasar_social_chat::ctx_queue_notice(
                    crate::quasar_gateway::NoticeScope::Character(target_char),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.CompletedNotice"),
                        value: comp_bytes,
                    },
                    ctx.now_unix(),
                );
            }

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.admin.ProgressResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 5. eve_public.career.goal.api.admin.CompleteRequest -> CompleteResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.career.goal.api.admin.CompleteRequest",
        Arc::new(move |ctx, any, dyn_msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let Some(target_char) = extract_target_character(dyn_msg, any, 2, true)
                .or_else(|| ctx.effective_character_id())
            else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let _ = mgr.mark_completed(target_char, &target_uuid, ctx.now_unix())?;
            mgr.save_to_db();

            let mut not_bytes = Vec::new();
            let mut id_bytes = Vec::new();
            encode_career_goal_identifier(&target_uuid, &mut id_bytes);
            encode_bytes_field(1, &id_bytes, &mut not_bytes);
            super::quasar_social_chat::ctx_queue_notice(
                crate::quasar_gateway::NoticeScope::Character(target_char),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.CompletedNotice"),
                    value: not_bytes,
                },
                ctx.now_unix(),
            );

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.admin.CompleteResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 6. eve_public.app.eveonline.career.goal.GetAllRequest -> GetAllResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.app.eveonline.career.goal.GetAllRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let goals = mgr.get_or_init_goals(cid);
            let mut out = Vec::new();
            for g in &goals {
                let mut gw = Vec::new();
                encode_career_goal_wire(g, &mut gw);
                encode_bytes_field(1, &gw, &mut out);
            }
            for (ms, reached, claimed) in mgr.get_character_milestones(cid) {
                let cp = mgr.career_points_for_character(cid, ms.career_id);
                let mut mw = Vec::new();
                encode_career_milestone_goal_wire(&ms, cp, reached, claimed, &mut mw);
                encode_bytes_field(1, &mw, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.career.goal.GetAllResponse"
                ),
                value: out,
            })
        }),
    );

    // 7. eve_public.app.eveonline.career.goal.ClaimRewardRequest -> ClaimRewardResponse
    let mgr = Arc::clone(&manager);
    let db_app_claim = Arc::clone(&db);
    router.register_handler(
        "eve_public.app.eveonline.career.goal.ClaimRewardRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            if target_uuid[0] == 0x43 && target_uuid[1] == 0x4D {
                let mut mid_bytes = [0u8; 4];
                mid_bytes.copy_from_slice(&target_uuid[12..16]);
                let mid = u32::from_be_bytes(mid_bytes);
                let _ = mgr.claim_milestone_reward(cid, mid, &db_app_claim)?;
            } else {
                let _ = mgr.claim_reward(cid, &target_uuid, &db_app_claim)?;
                mgr.save_to_db();
            }
            let mut id_bytes = Vec::new();
            encode_career_goal_identifier(&target_uuid, &mut id_bytes);
            let mut out = Vec::new();
            encode_bytes_field(1, &id_bytes, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.career.goal.ClaimRewardResponse"
                ),
                value: out,
            })
        }),
    );

    // 8. eve.career.goal.api.GetDefinitionsRequest (and .requests. alias) -> GetDefinitionsResponse
    for req_name in [
        "eve.career.goal.api.GetDefinitionsRequest",
        "eve.career.goal.api.requests.GetDefinitionsRequest",
    ] {
        let mgr_eve_defs = Arc::clone(&manager);
        router.register_handler(
            req_name,
            Arc::new(move |_ctx, _any, _dyn| {
                let defs = mgr_eve_defs.load_definitions();
                let mut out = Vec::new();
                for def in &defs {
                    let mut gw = Vec::new();
                    encode_internal_career_goal_definition_wire(def, &mut gw);
                    encode_bytes_field(1, &gw, &mut out);
                }
                for ms in &mgr_eve_defs.milestones {
                    let mut mw = Vec::new();
                    encode_internal_career_milestone_definition_wire(ms, &mut mw);
                    encode_bytes_field(1, &mw, &mut out);
                }
                Ok(GoogleAny {
                    type_url: format!(
                        "{TYPE_GOOGLEAPIS_PREFIX}eve.career.goal.api.GetDefinitionsResponse"
                    ),
                    value: out,
                })
            }),
        );
    }

    // 9. eve.career.goal.api.admin.GetAllByCharacterRequest (and .requests. alias) -> GetAllByCharacterResponse
    for req_name in [
        "eve.career.goal.api.admin.GetAllByCharacterRequest",
        "eve.career.goal.api.admin.requests.GetAllByCharacterRequest",
    ] {
        let mgr = Arc::clone(&manager);
        router.register_handler(
            req_name,
            Arc::new(move |ctx, any, dyn_msg| {
                crate::quasar_gateway::require_admin_role(ctx)?;
                let Some(target_char) = extract_target_character(dyn_msg, any, 1, true)
                    .or_else(|| ctx.effective_character_id())
                else {
                    return Err((STATUS_FORBIDDEN, "Active character required".into()));
                };
                let goals = mgr.get_or_init_goals(target_char);
                let mut out = Vec::new();
                for g in &goals {
                    let mut gw = Vec::new();
                    encode_career_goal_wire(g, &mut gw);
                    encode_bytes_field(1, &gw, &mut out);
                }
                for (ms, reached, claimed) in mgr.get_character_milestones(target_char) {
                    let cp = mgr.career_points_for_character(target_char, ms.career_id);
                    let mut mw = Vec::new();
                    encode_career_milestone_goal_wire(&ms, cp, reached, claimed, &mut mw);
                    encode_bytes_field(1, &mw, &mut out);
                }
                Ok(GoogleAny {
                    type_url: format!(
                        "{TYPE_GOOGLEAPIS_PREFIX}eve.career.goal.api.admin.GetAllByCharacterResponse"
                    ),
                    value: out,
                })
            }),
        );
    }

    // 10. eve.career.goal.api.admin.CompleteRequest (and .requests. alias) -> CompleteResponse
    for req_name in [
        "eve.career.goal.api.admin.CompleteRequest",
        "eve.career.goal.api.admin.requests.CompleteRequest",
    ] {
        let mgr = Arc::clone(&manager);
        router.register_handler(
            req_name,
            Arc::new(move |ctx, any, dyn_msg| {
                crate::quasar_gateway::require_admin_role(ctx)?;
                let Some(target_char) = extract_target_character(dyn_msg, any, 2, true)
                    .or_else(|| ctx.effective_character_id())
                else {
                    return Err((STATUS_FORBIDDEN, "Active character required".into()));
                };
                let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                    return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
                };
                let _ = mgr.mark_completed(target_char, &target_uuid, ctx.now_unix())?;
                mgr.save_to_db();
                Ok(GoogleAny {
                    type_url: format!(
                        "{TYPE_GOOGLEAPIS_PREFIX}eve.career.goal.api.admin.CompleteResponse"
                    ),
                    value: Vec::new(),
                })
            }),
        );
    }

    // 11. eve.character.career.goal.GetProgressRequest -> GetProgressResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.character.career.goal.GetProgressRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 2).or_else(|| extract_uuid(dyn_msg, 1))
            else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let goals = mgr.get_or_init_goals(cid);
            let g = goals
                .iter()
                .find(|g| g.uuid == target_uuid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Career goal not found".into()))?;

            let mut out = Vec::new();
            encode_varint_field(1, g.progress, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.character.career.goal.GetProgressResponse"
                ),
                value: out,
            })
        }),
    );

    // 12. eve.character.career.goal.HasClaimedRewardRequest -> HasClaimedRewardResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.character.career.goal.HasClaimedRewardRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 2).or_else(|| extract_uuid(dyn_msg, 1))
            else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let goals = mgr.get_or_init_goals(cid);
            let g = goals
                .iter()
                .find(|g| g.uuid == target_uuid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Career goal not found".into()))?;

            let mut out = Vec::new();
            encode_varint_field(1, if g.claimed { 1 } else { 0 }, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.character.career.goal.HasClaimedRewardResponse"
                ),
                value: out,
            })
        }),
    );
}

#[cfg(test)] #[path = "quasar_career_goals_tests.rs"]
mod quasar_career_goals_tests;

