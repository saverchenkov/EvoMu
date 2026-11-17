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
use eve_proto::pb;
#[cfg(test)]
use eve_proto::quasar::GoogleAny;
use eve_proto::quasar::{DynamicValue, WireMessage};
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

/// V24 AIR Career Program goal definitions loaded from `evomu_sde.sqlite` (`sde_career_goals`,
/// `sde_career_paths`), sourced from client `res:/staticdata/careerpaths.fsdbinary` and
/// `Character/CareerGoals/*` localization.
///
/// DERIVED elements (not present in V24 client local static files):
/// - Deterministic server UUIDs `[0x43, 0x47, career_order, ..., idx]` (not V24 catalog UUIDs).
/// - Per-goal Career Points (`5..=50`, `1,000 CP` total per career) and ISK payouts (`CP * 2,500 ISK`),
///   derived from `~/ai/eve-wiki/articles/AIR_Career_Program.md`.
/// - Standing target basis points (`10, 25, 50, 100`) and Career Agent mission ordinals (`1`).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CareerGoalDef {
    pub uuid: [u8; 16],
    pub career_id: u32, // 1: Explorer, 2: Industrialist, 3: Enforcer, 4: Soldier of Fortune (Attributes.Career)
    #[serde(default)]
    pub goal_label: String,
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
    /// Returns the authentic V24.01 localization message ID for `Attributes.name.sequential`,
    /// falling back to the career path's authentic `careerpaths.fsdbinary` `nameID` when unset.
    pub fn effective_name_message_id(&self) -> u32 {
        if self.name_message_id > 0 {
            return self.name_message_id;
        }
        CAREER_PATH_MESSAGE_IDS
            .iter()
            .find(|(cid, _, _)| *cid == self.career_id)
            .map(|(_, n, _)| *n)
            .unwrap_or(582_520)
    }

    /// Returns the authentic V24.01 localization message ID for `Attributes.description.sequential`,
    /// falling back to the career path's authentic `careerpaths.fsdbinary` `descriptionID` when unset.
    pub fn effective_description_message_id(&self) -> u32 {
        if self.description_message_id > 0 {
            return self.description_message_id;
        }
        CAREER_PATH_MESSAGE_IDS
            .iter()
            .find(|(cid, _, _)| *cid == self.career_id)
            .map(|(_, _, d)| *d)
            .unwrap_or(582_521)
    }

    /// Returns the canonical V24 `CAREER_PATH_GROUPS` activity group annotation key (`ui_annotations["group"]`).
    pub fn effective_group(&self) -> &'static str {
        const EXPLORER_GROUPS: &[(&str, &str, &str)] = &[
            ("scanning", "exploration", "Scanning"),
            ("hacking", "hacking", "Hacking"),
            ("advancedhacking", "advanced_hacking", "AdvancedHacking"),
            ("wormholes", "wormholes", "Wormholes"),
            ("gassites", "gas_sites", "GasSites"),
            ("combatsites", "combat_sites", "CombatSites"),
            ("navigation", "travel", "Navigation"),
            ("advancednavigation", "advanced_travel", "AdvancedNavigation"),
            ("projectdiscovery", "project_discovery", "ProjectDiscovery"),
        ];
        const INDUSTRIALIST_GROUPS: &[(&str, &str, &str)] = &[
            ("market", "market", "Market"),
            ("salvage", "salvage", "Salvage"),
            ("resourceharvesting", "mining", "ResourceHarvesting"),
            ("refining", "refining", "Refining"),
            ("manufacturing", "manufacturing", "Manufacturing"),
            ("distributionagent", "distribution_missions", "DistributionAgent"),
            ("miningagent", "mining_missions", "MiningAgent"),
            ("research", "research", "Research"),
        ];
        const ENFORCER_GROUPS: &[(&str, &str, &str)] = &[
            ("securityagent", "combat_missions", "SecurityAgent"),
            ("abyssaldeadspace", "abyssal", "AbyssalDeadspace"),
            ("combat", "combat", "Combat"),
            ("bounties", "bounties", "Bounties"),
            ("combatsites", "combat_sites", "CombatSites"),
            ("epicarc", "epic_arc", "EpicArc"),
            ("market", "market", "Market"),
            ("standings", "standings", "Standings"),
            ("loyaltypoints", "loyalty_points", "LoyaltyPoints"),
        ];
        const SOLDIER_GROUPS: &[(&str, &str, &str)] = &[
            ("social", "social", "Social"),
            ("electronicwarfare", "ewar", "ElectronicWarfare"),
            ("combat", "combat", "Combat"),
            ("pvp", "pvp", "PvP"),
            ("capacitorwarfare", "cap_warfare", "CapacitorWarfare"),
            ("factionwarfare", "faction_warfare", "FactionWarfare"),
            ("support", "remote_repair", "Support"),
            ("destruction", "destruction", "Destruction"),
            ("duels", "duels", "Duels"),
        ];
        const DEFAULT_CAREER_GROUP: &str = "CareerAgentMissions";
        const INDUSTRIALIST_DEFAULT_GROUP: &str = "IndustrialistCareerAgentsMissions";

        let norm: String = self
            .activity_name
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        let kind = self.activity_kind.to_ascii_lowercase();
        let (table, fallback): (&[(&str, &str, &str)], &'static str) = match self.career_id.clamp(1, 4) {
            1 => (EXPLORER_GROUPS, DEFAULT_CAREER_GROUP),
            2 => (INDUSTRIALIST_GROUPS, INDUSTRIALIST_DEFAULT_GROUP),
            3 => (ENFORCER_GROUPS, DEFAULT_CAREER_GROUP),
            _ => (SOLDIER_GROUPS, DEFAULT_CAREER_GROUP),
        };
        table
            .iter()
            .find(|&&(n, k, _)| norm == n || kind == k)
            .map(|&(_, _, out)| out)
            .unwrap_or(fallback)
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

/// DERIVED per-goal AIR Career Program points from `~/ai/eve-wiki/articles/AIR_Career_Program.md`
/// (`5..=50 CP` per goal, `1,000 CP` total per career path, `ISK = CP * 2,500`), because client
/// local `careerpaths.fsdbinary` and localization pickles do not carry per-goal CP or ISK rewards.
pub fn derived_wiki_career_goal_points(label: &str) -> Option<u32> {
    Some(match label {
        // Explorer (40 goals, 1,000 CP)
        "ExplorerCareerAgentMission1"
        | "ExplorerCareerAgentMission2"
        | "ExplorerCareerAgentMission3"
        | "ExplorerCareerAgentMission4"
        | "ExplorerCareerAgentMission5" => 15,
        "ExplorerScanSignature1" => 10,
        "ExplorerScanSignature10" => 15,
        "ExplorerScanSignature25" => 25,
        "ExplorerScanSignature50" => 50,
        "ExplorerHackContainer1" => 10,
        "ExplorerHackContainer5" => 15,
        "ExplorerHackContainer10" => 25,
        "ExplorerHackContainer15" => 30,
        "ExplorerHackContainer25" => 50,
        "ExplorerHackLowsecContainer20"
        | "ExplorerHackNullsecContainer20"
        | "ExplorerHackWhContainer20" => 50,
        "ExplorerScanWormholes1" => 10,
        "ExplorerScanWormholes5" => 15,
        "ExplorerScanWormholes10" => 20,
        "ExplorerScanWormholes15" => 30,
        "ExplorerEnterGasSites1" => 10,
        "ExplorerEnterGasSites5" => 15,
        "ExplorerEnterGasSites10" => 20,
        "ExplorerEnterGasSites15" => 30,
        "ExplorerEnterCombatSites1" => 10,
        "ExplorerEnterCombatSites5" => 15,
        "ExplorerEnterCombatSites10" => 20,
        "ExplorerEnterCombatSites15" => 30,
        "ExplorerGateJumps25" => 10,
        "ExplorerGateJumps50" => 20,
        "ExplorerGateJumps100" => 40,
        "ExplorerVisitHisec" => 30,
        "ExplorerVisitLowsec" => 40,
        "ExplorerVisitNullsec" => 50,
        "ExplorerProjectDiscoverySamples1" => 10,
        "ExplorerProjectDiscoverySamples5" => 15,
        "ExplorerProjectDiscoverySamples10" => 20,
        "ExplorerProjectDiscoverySamples25" => 35,
        "ExplorerProjectDiscoverySamples50" => 50,

        // Industrialist (46 goals, 1,000 CP)
        "IndustrialistEntrepreneurCareerAgentMission2"
        | "IndustrialistEntrepreneurCareerAgentMission4"
        | "IndustrialistEntrepreneurCareerAgentMission6"
        | "IndustrialistEntrepreneurCareerAgentMission8"
        | "IndustrialistEntrepreneurCareerAgentMission10"
        | "IndustrialistProducerCareerAgentMission2"
        | "IndustrialistProducerCareerAgentMission4"
        | "IndustrialistProducerCareerAgentMission6"
        | "IndustrialistProducerCareerAgentMission8"
        | "IndustrialistProducerCareerAgentMission10" => 20,
        "IndustrialistEarnMarketIsk50k" => 10,
        "IndustrialistEarnMarketIsk100k" => 15,
        "IndustrialistEarnMarketIsk250k" => 20,
        "IndustrialistEarnMarketIsk500k" => 25,
        "IndustrialistEarnMarketIsk1m" => 50,
        "IndustrialistSalvageWrecks10" => 10,
        "IndustrialistSalvageWrecks25" => 25,
        "IndustrialistSalvageWrecks50" => 50,
        "IndustrialistMineVeldspar"
        | "IndustrialistMineScordite"
        | "IndustrialistMinePyroxeres"
        | "IndustrialistMinePlagioclase" => 25,
        "IndustrialistMineKernite" => 50,
        "IndustrialistReprocessVeldspar" | "IndustrialistReprocessScordite" => 10,
        "IndustrialistReprocessPyroxeres" | "IndustrialistReprocessPlagioclase" => 15,
        "IndustrialistReprocessKernite" => 25,
        "IndustrialistManufactureShuttle" => 10,
        "IndustrialistManufactureFrigate" => 15,
        "IndustrialistManufactureDestroyer" => 25,
        "IndustrialistManufactureCruiser" => 50,
        "IndustrialistCourierMissions1" => 10,
        "IndustrialistCourierMissions5" => 15,
        "IndustrialistCourierMissions10" => 20,
        "IndustrialistCourierMissions15" => 25,
        "IndustrialistCourierMissions20" => 30,
        "IndustrialistMiningMissions1" => 10,
        "IndustrialistMiningMissions5" => 15,
        "IndustrialistMiningMissions10" => 20,
        "IndustrialistMiningMissions15" => 25,
        "IndustrialistMiningMissions20" => 30,
        "IndustrialistBuyBlueprint" | "IndustrialistCopyBlueprint" => 10,
        "IndustrialistResearchBlueprintME" | "IndustrialistResearchBlueprintTE" => 25,

        // Enforcer (49 goals, 1,000 CP)
        "EnforcerCareerAgentMission1"
        | "EnforcerCareerAgentMission2"
        | "EnforcerCareerAgentMission3"
        | "EnforcerCareerAgentMission4"
        | "EnforcerCareerAgentMission5"
        | "EnforcerCareerAgentMission6"
        | "EnforcerCareerAgentMission7"
        | "EnforcerCareerAgentMission8"
        | "EnforcerCareerAgentMission9"
        | "EnforcerCareerAgentMission10" => 10,
        "EnforcerLevel1SecurityMissions1" => 10,
        "EnforcerLevel1SecurityMissions5" => 15,
        "EnforcerLevel1SecurityMissions10" => 20,
        "EnforcerLevel1SecurityMissions15" => 25,
        "EnforcerLevel1SecurityMissions20" => 30,
        "EnforcerEnterTier0Abyss" => 10,
        "EnforcerCompleteTier0Abyss1" => 20,
        "EnforcerCompleteTier0Abyss5" => 25,
        "EnforcerKillTriglavians30" => 30,
        "EnforcerDestroyNpc100" => 10,
        "EnforcerDestroyNpc150" => 20,
        "EnforcerDestroyNpc200" => 30,
        "EnforcerDestroyNpc250" => 40,
        "EnforcerDestroyNpc300" => 50,
        "EnforcerEarnBountyIsk250k" => 10,
        "EnforcerEarnBountyIsk500k" => 20,
        "EnforcerEarnBountyIsk1m" => 30,
        "EnforcerEarnBountyIsk5m" => 50,
        "EnforcerCompleteCombatSites5" => 10,
        "EnforcerCompleteCombatSites15" => 25,
        "EnforcerCompleteCombatSites25" => 50,
        "EnforcerEpicArcMissions1" => 5,
        "EnforcerEpicArcMissions5" => 10,
        "EnforcerEpicArcMissions10" => 15,
        "EnforcerEpicArcMissions20" => 20,
        "EnforcerEpicArcComplete" => 50,
        "EnforcerSpendMarketIsk50k" => 5,
        "EnforcerSpendMarketIsk250k" => 15,
        "EnforcerSpendMarketIsk500k" => 25,
        "EnforcerSpendMarketIsk1m" => 50,
        "EnforcerGetStandingTo01" => 10,
        "EnforcerGetStandingTo025" => 15,
        "EnforcerGetStandingTo05" => 25,
        "EnforcerGetStandingToMissionsLevel2" => 30,
        "EnforcerEarnLp50" => 10,
        "EnforcerEarnLp100" => 15,
        "EnforcerEarnLp250" => 20,
        "EnforcerEarnLp500" => 25,
        "EnforcerSpendLp300" => 25,

        // Soldier of Fortune (53 goals, 1,000 CP)
        "SofCareerAgentMission1"
        | "SofCareerAgentMission2"
        | "SofCareerAgentMission3"
        | "SofCareerAgentMission4"
        | "SofCareerAgentMission5"
        | "SofCareerAgentMission6"
        | "SofCareerAgentMission7"
        | "SofCareerAgentMission8"
        | "SofCareerAgentMission9"
        | "SofCareerAgentMission10" => 10,
        "SofSendChatMessage" => 10,
        "SofJoinFleet" => 25,
        "SofJoinCorporation" => 30,
        "SofSendMail" => 10,
        "SofAddContact" => 10,
        "SofWebify"
        | "SofTargetPaint"
        | "SofWarpDisrupt"
        | "SofWarpScramble"
        | "SofWeaponDisrupt" => 20,
        "SofDestroyNpcInFrigate" => 20,
        "SofDestroyNpcInDestroyer" => 30,
        "SofDestroyNpcInCruiser" => 40,
        "SofKillCapsuleers2" => 20,
        "SofKillCapsuleers3" => 30,
        "SofKillCapsuleers4" => 40,
        "SofKillCapsuleers5" => 50,
        "SofDrainCapacitor50" => 10,
        "SofDrainCapacitor100" => 20,
        "SofDrainCapacitor250" => 30,
        "SofNeutralizeCapacitor100" => 10,
        "SofNeutralizeCapacitor500" => 20,
        "SofNeutralizeCapacitor2000" => 30,
        "SofJoinFW" => 10,
        "SofKillFwCapsuleeer1" => 25,
        "SofLevel1FwMission1" => 10,
        "SofDefendFwComplexNovice" => 30,
        "SofCaptureFwComplexNovice" => 30,
        "SofRemoteBoostShieldHp500" => 10,
        "SofRemoteBoostShieldHp1000" => 15,
        "SofRemoteBoostShieldHp2500" => 25,
        "SofRemoteRepArmorHp500" => 10,
        "SofRemoteRepArmorHp1000" => 15,
        "SofRemoteRepArmorHp2500" => 25,
        "SofLoseShip1" => 5,
        "SofLoseShip5" => 10,
        "SofLoseShip10" => 15,
        "SofLoseShip15" => 25,
        "SofLoseShip20" => 30,
        "SofDuelChallenge" => 5,
        "SofDuelWin1" => 10,
        "SofDuelWin3" => 20,
        "SofDuelWin5" => 40,
        _ => return None,
    })
}

fn career_goal_def_from_sde(row: &eve_sde::CareerGoalDefinitionDef) -> CareerGoalDef {
    let career_points = derived_wiki_career_goal_points(&row.label).unwrap_or(15);
    let isk_reward = i64::from(career_points) * 2_500;
    let threat = if career_points <= 15 {
        1
    } else if career_points <= 25 {
        2
    } else {
        3
    };
    CareerGoalDef {
        uuid: row.goal_uuid,
        career_id: row.career_id,
        goal_label: row.label.clone(),
        activity_name: row.activity_name.clone(),
        activity_kind: row.activity_kind.clone(),
        name: row.name.clone(),
        description: row.description.clone(),
        target: row.target_progress.max(1),
        career_points,
        isk_reward,
        sp_reward: row.sp_reward as f64,
        name_message_id: row.name_message_id,
        description_message_id: row.description_message_id,
        threat,
        duration: Some(career_points * 2),
        aura_text_id: (row.aura_text_id > 0).then_some(row.aura_text_id),
        agency_link_text_id: (row.agency_link_text_id > 0).then_some(row.agency_link_text_id),
        agency_screen: Some("HOMEPAGE".into()),
        order: Some(row.sort_order.max(1)),
        _provenance: Some(
            "V24 client Character/CareerGoals/* + careerpaths.fsdbinary in evomu_sde.sqlite; synthesized [0x43, 0x47, ...] UUIDs, standing/mission target scaling, and per-goal CP/ISK payouts (5..=50 CP, CP*2500 ISK) are DERIVED".into(),
        ),
    }
}

pub fn default_career_goal_definitions_for_sde(sde: Option<&eve_sde::SdeRegistry>) -> Vec<CareerGoalDef> {
    if let Some(reg) = sde {
        return reg
            .career_goal_definitions
            .iter()
            .map(career_goal_def_from_sde)
            .collect();
    }
    eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
        .ok()
        .map(|reg| {
            reg.career_goal_definitions
                .iter()
                .map(career_goal_def_from_sde)
                .collect()
        })
        .unwrap_or_default()
}

pub fn default_career_goal_definitions() -> Vec<CareerGoalDef> {
    default_career_goal_definitions_for_sde(None)
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
    pub seen_regions: RwLock<HashMap<u32, HashMap<String, HashSet<u32>>>>,
    pub milestones: Vec<CareerMilestoneDef>,
    db: Option<Arc<WorldRepository>>,
    sde: Option<Arc<eve_sde::SdeRegistry>>,
}

fn is_stale_synthesized_loc_id(def: &CareerGoalDef) -> bool {
    def.name_message_id == 0
        || (582_531..=582_689).contains(&def.name_message_id)
        || (665_435..=665_600).contains(&def.name_message_id)
        || def.description_message_id == 0
        || (696_260..=696_450).contains(&def.description_message_id)
        || def.aura_text_id.is_none()
}

fn sync_def_localization_from(dst: &mut CareerGoalDef, src: &CareerGoalDef) {
    dst.name_message_id = src.name_message_id;
    dst.description_message_id = src.description_message_id;
    dst.aura_text_id = src.aura_text_id;
    dst.agency_link_text_id = src.agency_link_text_id;
    dst.agency_screen = src.agency_screen.clone();
    dst.duration = src.duration;
    dst.order = src.order;
    dst._provenance = src._provenance.clone();
}

fn sync_def_derived_fields_from(dst: &mut CareerGoalDef, src: &CareerGoalDef) {
    if dst.goal_label.is_empty() && !src.goal_label.is_empty() {
        dst.goal_label = src.goal_label.clone();
    }
    if !src.goal_label.is_empty() && dst.goal_label == src.goal_label {
        dst.target = src.target;
        dst.career_points = src.career_points;
        dst.isk_reward = src.isk_reward;
        dst.threat = src.threat;
        dst.duration = src.duration;
    }
}

fn ensure_character_career_goals_with_defs(
    goals: &mut Vec<CharacterCareerGoal>,
    defs: &[CareerGoalDef],
) {
    goals.retain(|g| defs.iter().any(|d| d.uuid == g.uuid));
    for g in goals.iter_mut() {
        if let Some(def) = defs.iter().find(|d| d.uuid == g.uuid) {
            if g.def.activity_kind.is_empty() {
                g.def.activity_kind = def.activity_kind.clone();
            }
            sync_def_derived_fields_from(&mut g.def, def);
            if is_stale_synthesized_loc_id(&g.def) && !is_stale_synthesized_loc_id(def) {
                sync_def_localization_from(&mut g.def, def);
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

fn load_persisted_career_state(
    snap: &eve_db::WorldStateSnapshot,
    default_defs: &[CareerGoalDef],
) -> (
    Vec<CareerGoalDef>,
    HashMap<u32, Vec<CharacterCareerGoal>>,
    HashMap<u32, HashSet<u32>>,
    HashMap<u32, HashMap<String, HashSet<u32>>>,
) {
    let defs = snap
        .quasar_state_blobs
        .get("quasar_career_goal_defs")
        .and_then(|json| serde_json::from_str::<Vec<CareerGoalDef>>(json).ok())
        .map(|mut loaded_defs| {
            for def in &mut loaded_defs {
                if let Some(auth) = default_defs.iter().find(|d| d.uuid == def.uuid) {
                    sync_def_derived_fields_from(def, auth);
                    if is_stale_synthesized_loc_id(def) {
                        sync_def_localization_from(def, auth);
                    }
                }
            }
            loaded_defs
        })
        .unwrap_or_else(|| default_defs.to_vec());
    let mut loaded = snap
        .quasar_state_blobs
        .get("quasar_career_goals")
        .and_then(|json| {
            serde_json::from_str::<HashMap<u32, Vec<CharacterCareerGoal>>>(json).ok()
        })
        .unwrap_or_default();
    for goals in loaded.values_mut() {
        ensure_character_career_goals_with_defs(goals, &defs);
    }
    let mut milestones = snap
        .quasar_state_blobs
        .get("quasar_career_milestones")
        .and_then(|json| serde_json::from_str::<HashMap<u32, HashSet<u32>>>(json).ok())
        .unwrap_or_default();
    let valid_mids: HashSet<u32> = default_career_milestones()
        .into_iter()
        .map(|m| m.milestone_id)
        .collect();
    for (&cid, npe) in &snap.npe_progress {
        if let Ok(u32_cid) = u32::try_from(cid) {
            for &gid in &npe.claimed_goals {
                if valid_mids.contains(&gid) {
                    milestones.entry(u32_cid).or_default().insert(gid);
                }
            }
        }
    }
    let seen_regions = snap
        .quasar_state_blobs
        .get("quasar_career_seen_regions")
        .and_then(|json| {
            serde_json::from_str::<HashMap<u32, HashMap<String, HashSet<u32>>>>(json).ok()
        })
        .unwrap_or_default();
    (defs, loaded, milestones, seen_regions)
}

impl CareerGoalManager {
    pub fn new(db: Option<Arc<WorldRepository>>) -> Self {
        Self::new_with_sde(db, None)
    }

    pub fn new_with_sde(
        db: Option<Arc<WorldRepository>>,
        sde: Option<Arc<eve_sde::SdeRegistry>>,
    ) -> Self {
        let default_defs = default_career_goal_definitions_for_sde(sde.as_deref());
        let (defs, loaded, loaded_milestones, loaded_regions) = db
            .as_ref()
            .map(|d| d.with_snapshot(|s| load_persisted_career_state(s, &default_defs)))
            .unwrap_or_else(|| {
                (
                    default_defs,
                    HashMap::new(),
                    HashMap::new(),
                    HashMap::new(),
                )
            });

        Self {
            definitions: RwLock::new(defs),
            character_career_goals: RwLock::new(loaded),
            claimed_milestones: RwLock::new(loaded_milestones),
            seen_regions: RwLock::new(loaded_regions),
            milestones: default_career_milestones(),
            db,
            sde,
        }
    }

    pub fn load_definitions(&self) -> Vec<CareerGoalDef> {
        let defs = self.definitions.read().unwrap_or_else(|e| e.into_inner());
        if defs.is_empty() {
            default_career_goal_definitions_for_sde(self.sde.as_deref())
        } else {
            defs.clone()
        }
    }

    pub fn configure_definition(&self, def: CareerGoalDef) {
        let defs = {
            let mut d = self.definitions.write().unwrap_or_else(|e| e.into_inner());
            if d.is_empty() {
                *d = default_career_goal_definitions_for_sde(self.sde.as_deref());
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
            let sr = self.seen_regions.read().unwrap_or_else(|e| e.into_inner());
            let defs_json = if defs.is_empty() {
                None
            } else {
                serde_json::to_string(&*defs).ok()
            };
            let goals_json = serde_json::to_string(&*map).ok();
            let milestones_json = serde_json::to_string(&*cm).ok();
            let regions_json = serde_json::to_string(&*sr).ok();
            let already = db.with_snapshot(|snap| {
                defs_json.as_ref().is_none_or(|j| snap.quasar_state_blobs.get("quasar_career_goal_defs") == Some(j))
                    && goals_json.as_ref().is_none_or(|j| snap.quasar_state_blobs.get("quasar_career_goals") == Some(j))
                    && milestones_json.as_ref().is_none_or(|j| snap.quasar_state_blobs.get("quasar_career_milestones") == Some(j))
                    && regions_json.as_ref().is_none_or(|j| snap.quasar_state_blobs.get("quasar_career_seen_regions") == Some(j))
            });
            if already {
                return;
            }
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
                if let Some(r_json) = regions_json {
                    snap.quasar_state_blobs.insert("quasar_career_seen_regions".into(), r_json);
                }
                Ok::<(), String>(())
            });
        }
    }

    pub fn rehydrate_from_db(&self) {
        let Some(ref db) = self.db else { return };
        let default_defs = default_career_goal_definitions_for_sde(self.sde.as_deref());
        let (defs, loaded, loaded_milestones, loaded_regions) =
            db.with_snapshot(|s| load_persisted_career_state(s, &default_defs));
        *self.definitions.write().unwrap_or_else(|e| e.into_inner()) = defs;
        *self
            .character_career_goals
            .write()
            .unwrap_or_else(|e| e.into_inner()) = loaded;
        *self
            .claimed_milestones
            .write()
            .unwrap_or_else(|e| e.into_inner()) = loaded_milestones;
        *self.seen_regions.write().unwrap_or_else(|e| e.into_inner()) = loaded_regions;
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
}

/// Structured gameplay event context for evaluating AIR Career Program objective predicates.
#[derive(Debug, Clone, Default)]
pub struct CareerGameplayEvent<'a> {
    pub activity_kind: &'a str,
    /// Item or product `type_id` (harvested ore, manufactured item, reprocessed ore).
    pub type_id: Option<u32>,
    /// Item/ore/ship SDE `group_id` (e.g. `462` Veldspar, `25` Frigate).
    pub group_id: Option<u32>,
    /// Pilot active ship `group_id` when destroying an NPC (ship-class kill goals).
    pub ship_group_id: Option<u32>,
    pub solar_system_id: Option<u32>,
    pub region_id: Option<u32>,
    pub security_status: Option<f64>,
    pub is_wormhole: Option<bool>,
    /// Effective corporation standing in basis points (`0.1` -> `10`, `1.0` -> `100`).
    pub standing_bps: Option<u64>,
    /// Career Agent division (`"explorer"`, `"entrepreneur"`, `"producer"`, `"enforcer"`, `"soldier_of_fortune"`).
    pub mission_division: Option<&'a str>,
    /// Career Agent mission ordinal (`1..=10`).
    pub mission_ordinal: Option<u32>,
    /// Discriminator within shared `activity_kind` buckets (`"earn_isk"`, `"spend_isk"`, etc.).
    pub sub_kind: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CareerSecBand {
    HighSec,
    LowSec,
    NullSec,
    Wormhole,
}

enum CareerObjectiveMatch {
    AddDelta,
    DistinctRegion(u32),
    Threshold(u64),
}

fn event_sec_band(ev: &CareerGameplayEvent<'_>) -> Option<CareerSecBand> {
    if ev.is_wormhole == Some(true) || ev.solar_system_id.is_some_and(|id| id >= 31_000_000) {
        return Some(CareerSecBand::Wormhole);
    }
    let sec = ev.security_status?;
    if sec >= 0.45 {
        Some(CareerSecBand::HighSec)
    } else if sec > 0.0 {
        Some(CareerSecBand::LowSec)
    } else {
        Some(CareerSecBand::NullSec)
    }
}

fn required_ore_group(label: &str) -> Option<u32> {
    const ORE_GROUPS: &[(&str, u32)] = &[
        ("Veldspar", 462),
        ("Scordite", 460),
        ("Pyroxeres", 459),
        ("Plagioclase", 458),
        ("Kernite", 457),
    ];
    ORE_GROUPS
        .iter()
        .find(|&&(suffix, _)| label.ends_with(suffix))
        .map(|&(_, gid)| gid)
}

fn matches_ship_hull(label: &str, group_id: Option<u32>) -> bool {
    const SHUTTLE_GROUPS: &[u32] = &[31];
    const FRIGATE_GROUPS: &[u32] = &[25, 324, 830, 831, 834, 893, 1022, 1283, 1527];
    const DESTROYER_GROUPS: &[u32] = &[420, 541, 1305, 1534];
    const CRUISER_GROUPS: &[u32] = &[26, 358, 832, 833, 894, 906, 963, 1972];
    let Some(gid) = group_id else {
        return false;
    };
    let allowed = if label.ends_with("Shuttle") {
        SHUTTLE_GROUPS
    } else if label.ends_with("Frigate") {
        FRIGATE_GROUPS
    } else if label.ends_with("Destroyer") {
        DESTROYER_GROUPS
    } else if label.ends_with("Cruiser") {
        CRUISER_GROUPS
    } else {
        return false;
    };
    allowed.contains(&gid)
}

fn parse_career_mission_predicate(label: &str) -> Option<(&'static str, u32)> {
    const PREFIXES: &[(&str, &str)] = &[
        ("ExplorerCareerAgentMission", "explorer"),
        ("IndustrialistEntrepreneurCareerAgentMission", "entrepreneur"),
        ("IndustrialistProducerCareerAgentMission", "producer"),
        ("EnforcerCareerAgentMission", "enforcer"),
        ("SofCareerAgentMission", "soldier_of_fortune"),
    ];
    for &(prefix, division) in PREFIXES {
        if let Some(rest) = label.strip_prefix(prefix) {
            if let Ok(ord) = rest.parse::<u32>() {
                return Some((division, ord));
            }
        }
    }
    None
}

fn required_sub_kind(label: &str) -> Option<&'static str> {
    Some(match label {
        "IndustrialistBuyBlueprint" => "buy_blueprint",
        "IndustrialistCopyBlueprint" => "copy_blueprint",
        "IndustrialistResearchBlueprintME" => "research_me",
        "IndustrialistResearchBlueprintTE" => "research_te",
        "EnforcerEnterTier0Abyss" => "enter_tier0",
        "EnforcerCompleteTier0Abyss1" | "EnforcerCompleteTier0Abyss5" => "complete_tier0",
        "EnforcerKillTriglavians30" => "kill_triglavians",
        "ExplorerEnterCombatSites1"
        | "ExplorerEnterCombatSites5"
        | "ExplorerEnterCombatSites10"
        | "ExplorerEnterCombatSites15" => "enter_combat_site",
        "EnforcerCompleteCombatSites5"
        | "EnforcerCompleteCombatSites15"
        | "EnforcerCompleteCombatSites25" => "complete_combat_site",
        "EnforcerEpicArcMissions1"
        | "EnforcerEpicArcMissions5"
        | "EnforcerEpicArcMissions10"
        | "EnforcerEpicArcMissions20" => "epic_arc_mission",
        "EnforcerEpicArcComplete" => "epic_arc_complete",
        l if l.starts_with("IndustrialistEarnMarketIsk") => "earn_isk",
        l if l.starts_with("EnforcerSpendMarketIsk") => "spend_isk",
        l if l.starts_with("EnforcerEarnLp") => "earn_lp",
        "EnforcerSpendLp300" => "spend_lp",
        "SofSendChatMessage" => "chat_message",
        "SofJoinFleet" => "join_fleet",
        "SofJoinCorporation" => "join_corp",
        "SofSendMail" => "send_mail",
        "SofAddContact" => "add_contact",
        "SofWebify" => "webify",
        "SofTargetPaint" => "target_paint",
        "SofWarpDisrupt" => "warp_disrupt",
        "SofWarpScramble" => "warp_scramble",
        "SofWeaponDisrupt" => "weapon_disrupt",
        l if l.starts_with("SofDrainCapacitor") => "nosferatu",
        l if l.starts_with("SofNeutralizeCapacitor") => "neutralizer",
        "SofJoinFW" => "join_fw",
        "SofKillFwCapsuleeer1" => "kill_fw_capsuleer",
        "SofLevel1FwMission1" => "level1_fw_mission",
        "SofDefendFwComplexNovice" => "defend_scout_complex",
        "SofCaptureFwComplexNovice" => "capture_scout_complex",
        l if l.starts_with("SofRemoteBoostShieldHp") => "shield_boost",
        l if l.starts_with("SofRemoteRepArmorHp") => "armor_repair",
        "SofDuelChallenge" => "challenge_duel",
        "SofDuelWin1" | "SofDuelWin3" | "SofDuelWin5" => "participate_duel",
        _ => return None,
    })
}

fn evaluate_career_objective(
    goal: &CharacterCareerGoal,
    ev: &CareerGameplayEvent<'_>,
) -> Option<CareerObjectiveMatch> {
    if goal.def.activity_kind != ev.activity_kind || goal.completed {
        return None;
    }
    let label = goal.def.goal_label.as_str();
    if label.is_empty() {
        return Some(CareerObjectiveMatch::AddDelta);
    }

    if matches!(ev.activity_kind, "mining" | "refining") {
        let req_gid = required_ore_group(label)?;
        return (ev.group_id == Some(req_gid)).then_some(CareerObjectiveMatch::AddDelta);
    }

    if ev.activity_kind == "manufacturing" {
        return matches_ship_hull(label, ev.group_id).then_some(CareerObjectiveMatch::AddDelta);
    }

    if label.starts_with("SofDestroyNpcIn") {
        return matches_ship_hull(label, ev.ship_group_id).then_some(CareerObjectiveMatch::AddDelta);
    }

    if ev.activity_kind == "advanced_travel" {
        let expected_band = match label {
            "ExplorerVisitHisec" => CareerSecBand::HighSec,
            "ExplorerVisitLowsec" => CareerSecBand::LowSec,
            "ExplorerVisitNullsec" => CareerSecBand::NullSec,
            _ => return None,
        };
        if event_sec_band(ev) != Some(expected_band) {
            return None;
        }
        let region_id = ev.region_id?;
        return Some(CareerObjectiveMatch::DistinctRegion(region_id));
    }

    if ev.activity_kind == "advanced_hacking" {
        let expected_band = match label {
            "ExplorerHackLowsecContainer20" => CareerSecBand::LowSec,
            "ExplorerHackNullsecContainer20" => CareerSecBand::NullSec,
            "ExplorerHackWhContainer20" => CareerSecBand::Wormhole,
            _ => return None,
        };
        return (event_sec_band(ev) == Some(expected_band))
            .then_some(CareerObjectiveMatch::AddDelta);
    }

    if ev.activity_kind == "standings" {
        let bps = ev.standing_bps?;
        return Some(CareerObjectiveMatch::Threshold(bps));
    }

    if let Some((exp_div, exp_ord)) = parse_career_mission_predicate(label) {
        let div_ok = ev
            .mission_division
            .is_some_and(|d| d.eq_ignore_ascii_case(exp_div));
        return (div_ok && ev.mission_ordinal == Some(exp_ord))
            .then_some(CareerObjectiveMatch::AddDelta);
    }

    if let Some(req_sub) = required_sub_kind(label) {
        return (ev.sub_kind == Some(req_sub)).then_some(CareerObjectiveMatch::AddDelta);
    }

    Some(CareerObjectiveMatch::AddDelta)
}

/// Resolves solar system metadata (`region_id`, `security_status`, `is_wormhole`) from SDE into
/// a [`CareerGameplayEvent`].
pub(crate) fn solar_system_career_event<'a>(
    sde: &eve_sde::SdeRegistry,
    activity_kind: &'a str,
    sys_id: u32,
) -> CareerGameplayEvent<'a> {
    let sys_opt = sde.solar_systems.get(&sys_id);
    let is_wh = sys_id >= 31_000_000
        || sys_opt.is_some_and(|s| {
            s.wormhole_class_id
                .or_else(|| {
                    sde.constellations
                        .get(&s.constellation_id)
                        .and_then(|c| c.wormhole_class_id)
                })
                .or_else(|| sde.regions.get(&s.region_id).and_then(|r| r.wormhole_class_id))
                .is_some()
        });
    CareerGameplayEvent {
        activity_kind,
        solar_system_id: Some(sys_id),
        region_id: sys_opt.map(|s| s.region_id),
        security_status: sys_opt.map(|s| s.security_status),
        is_wormhole: Some(is_wh),
        ..Default::default()
    }
}

/// Resolves a character's currently active ship SDE `group_id`, if mounted in a valid ship.
pub(crate) fn character_active_ship_group_id(
    db: &WorldRepository,
    sde: &eve_sde::SdeRegistry,
    char_id: i64,
) -> Option<u32> {
    let ship_type_id = db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id)?;
        (ch.active_ship_id > 0)
            .then(|| snap.items.get(&ch.active_ship_id).map(|it| it.type_id))?
    })?;
    sde.types.get(&ship_type_id).map(|t| t.group_id)
}

impl CareerGoalManager {
    pub fn on_gameplay_event(
        &self,
        char_id: u32,
        activity_kind: &str,
        delta: u64,
        gateway: &PublicGatewayRouter,
    ) {
        self.on_career_gameplay_event(
            char_id,
            CareerGameplayEvent {
                activity_kind,
                ..Default::default()
            },
            delta,
            gateway,
        );
    }

    pub fn on_career_gameplay_event(
        &self,
        char_id: u32,
        ev: CareerGameplayEvent<'_>,
        delta: u64,
        gateway: &PublicGatewayRouter,
    ) {
        crate::quasar_gateway::isolate_side_effect("quasar_career_goals.on_gameplay_event", || {
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
                let mut seen_regions = self
                    .seen_regions
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                let goals = map.entry(char_id).or_default();
                self.ensure_goals_for_list(goals);

                for g in goals.iter_mut() {
                    let Some(how) = evaluate_career_objective(g, &ev) else {
                        continue;
                    };
                    let prev = g.completed;
                    let prev_progress = g.progress;
                    match how {
                        CareerObjectiveMatch::AddDelta => {
                            if delta == 0 {
                                continue;
                            }
                            g.progress = g.progress.saturating_add(delta);
                        }
                        CareerObjectiveMatch::DistinctRegion(region_id) => {
                            let label_key = if g.def.goal_label.is_empty() {
                                format!("{:032x}", u128::from_be_bytes(g.uuid))
                            } else {
                                g.def.goal_label.clone()
                            };
                            let visited = seen_regions
                                .entry(char_id)
                                .or_default()
                                .entry(label_key)
                                .or_default();
                            if !visited.insert(region_id) {
                                continue;
                            }
                            g.progress = (visited.len() as u64).min(g.def.target);
                        }
                        CareerObjectiveMatch::Threshold(bps) => {
                            let next = g.progress.max(bps.min(g.def.target));
                            if next <= g.progress {
                                continue;
                            }
                            g.progress = next;
                        }
                    }
                    if g.progress == prev_progress {
                        continue;
                    }
                    if g.progress >= g.def.target {
                        g.completed = true;
                        g.completed_time = Some(now_unix);
                    }
                    let just_completed = !prev && g.completed;
                    progressed.push((g.uuid, g.progress, just_completed));
                }
            }
            if !progressed.is_empty() {
                self.save_to_db();
            }

            for (uuid, new_progress, just_completed) in progressed {
                let _ = gateway.publish_pb_notice(
                    crate::quasar_gateway::NoticeScope::Character(char_id),
                    "quasar_career_goals.on_gameplay_event.progressed",
                    pb::message("eve_public.career.goal.api.ProgressedNotice")
                        .set("goal.uuid", uuid.to_vec())
                        .set("amount", new_progress.min(u64::from(u32::MAX)) as u32),
                    now_unix,
                );

                if just_completed {
                    let _ = gateway.publish_pb_notice(
                        crate::quasar_gateway::NoticeScope::Character(char_id),
                        "quasar_career_goals.on_gameplay_event.completed",
                        pb::message("eve_public.career.goal.api.CompletedNotice")
                            .set("goal.uuid", uuid.to_vec()),
                        now_unix,
                    );
                }
            }
        });
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
            let now_unix = db.clock().now_unix_secs();
            let credit_res = db.try_with_mut_snapshot(|snap| {
                let isk_cents = IskCents::from_isk(isk);
                let journal = (isk > 0).then(|| {
                    (
                        "CAREER_PROGRAM_REWARD",
                        format!("AIR Career Program goal reward ({})", res_goal.def.name),
                        Some((170, 1_000_125, now_unix)),
                    )
                });
                crate::domain::career_goals::credit_character_reward_in_snap(
                    snap,
                    character_id as i64,
                    isk_cents,
                    sp,
                    0,
                    journal,
                )
                .map(|_| ())
                .map_err(|e| e.message)
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
            crate::domain::career_goals::claim_career_goal_or_milestone_in_snap(
                snap,
                self,
                character_id as i64,
                milestone_id,
                u64::from(cp),
                u64::from(milestone.required_points),
                milestone.sp_reward,
            )
            .map(|_| ())
            .map_err(|e| {
                if e.kind == crate::domain::DomainErrorKind::Forbidden {
                    (STATUS_FORBIDDEN, e.message)
                } else {
                    (STATUS_BAD_REQUEST, "Unable to credit career milestone reward".into())
                }
            })
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
            let mut resp = pb::message("eve_public.career.goal.api.GetAllResponse");
            for g in &goals {
                let mut gw = Vec::new();
                encode_career_goal_wire(g, &mut gw);
                resp = resp.add("goals", pb::raw_message(&gw));
            }
            for (ms, reached, claimed) in mgr.get_character_milestones(cid) {
                let cp = mgr.career_points_for_character(cid, ms.career_id);
                let mut mw = Vec::new();
                encode_career_milestone_goal_wire(&ms, cp, reached, claimed, &mut mw);
                resp = resp.add("goals", pb::raw_message(&mw));
            }
            resp.to_any_or_internal_err()
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
                let mut resp = pb::message("eve_public.career.goal.api.GetDefinitionsResponse");
                for def in &defs {
                    let mut gw = Vec::new();
                    encode_career_goal_definition_wire(def, &mut gw);
                    resp = resp.add("goals", pb::raw_message(&gw));
                }
                for ms in &mgr_defs.milestones {
                    let mut mw = Vec::new();
                    encode_career_milestone_definition_wire(ms, &mut mw);
                    resp = resp.add("goals", pb::raw_message(&mw));
                }
                resp.to_any_or_internal_err()
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
            pb::message("eve_public.career.goal.api.ClaimRewardResponse").to_any_or_internal_err()
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

            crate::quasar_gateway::queue_pb_notice(
                crate::quasar_gateway::NoticeScope::Character(target_char),
                "quasar_career_goals.admin_progress.progressed",
                pb::message("eve_public.career.goal.api.ProgressedNotice")
                    .set("goal.uuid", target_uuid.to_vec())
                    .set("amount", goal.progress.min(u64::from(u32::MAX)) as u32),
                ctx.now_unix(),
            );

            if just_completed {
                crate::quasar_gateway::queue_pb_notice(
                    crate::quasar_gateway::NoticeScope::Character(target_char),
                    "quasar_career_goals.admin_progress.completed",
                    pb::message("eve_public.career.goal.api.CompletedNotice")
                        .set("goal.uuid", target_uuid.to_vec()),
                    ctx.now_unix(),
                );
            }

            pb::message("eve_public.career.goal.api.admin.ProgressResponse").to_any_or_internal_err()
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

            crate::quasar_gateway::queue_pb_notice(
                crate::quasar_gateway::NoticeScope::Character(target_char),
                "quasar_career_goals.admin_complete.completed",
                pb::message("eve_public.career.goal.api.CompletedNotice")
                    .set("goal.uuid", target_uuid.to_vec()),
                ctx.now_unix(),
            );

            pb::message("eve_public.career.goal.api.admin.CompleteResponse").to_any_or_internal_err()
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
            let mut resp = pb::message("eve_public.app.eveonline.career.goal.GetAllResponse");
            for g in &goals {
                let mut gw = Vec::new();
                encode_career_goal_wire(g, &mut gw);
                resp = resp.add("goals", pb::raw_message(&gw));
            }
            for (ms, reached, claimed) in mgr.get_character_milestones(cid) {
                let cp = mgr.career_points_for_character(cid, ms.career_id);
                let mut mw = Vec::new();
                encode_career_milestone_goal_wire(&ms, cp, reached, claimed, &mut mw);
                resp = resp.add("goals", pb::raw_message(&mw));
            }
            resp.to_any_or_internal_err()
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
            pb::message("eve_public.app.eveonline.career.goal.ClaimRewardResponse")
                .set("goal.uuid", target_uuid.to_vec())
                .to_any_or_internal_err()
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
                let mut resp = pb::message("eve.career.goal.api.GetDefinitionsResponse");
                for def in &defs {
                    let mut gw = Vec::new();
                    encode_internal_career_goal_definition_wire(def, &mut gw);
                    resp = resp.add("goals", pb::raw_message(&gw));
                }
                for ms in &mgr_eve_defs.milestones {
                    let mut mw = Vec::new();
                    encode_internal_career_milestone_definition_wire(ms, &mut mw);
                    resp = resp.add("goals", pb::raw_message(&mw));
                }
                resp.to_any_or_internal_err()
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
                let mut resp = pb::message("eve.career.goal.api.admin.GetAllByCharacterResponse");
                for g in &goals {
                    let mut gw = Vec::new();
                    encode_career_goal_wire(g, &mut gw);
                    resp = resp.add("goals", pb::raw_message(&gw));
                }
                for (ms, reached, claimed) in mgr.get_character_milestones(target_char) {
                    let cp = mgr.career_points_for_character(target_char, ms.career_id);
                    let mut mw = Vec::new();
                    encode_career_milestone_goal_wire(&ms, cp, reached, claimed, &mut mw);
                    resp = resp.add("goals", pb::raw_message(&mw));
                }
                resp.to_any_or_internal_err()
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
                pb::message("eve.career.goal.api.admin.CompleteResponse").to_any_or_internal_err()
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

            pb::message("eve.character.career.goal.GetProgressResponse")
                .set("progress", g.progress.min(u64::from(u32::MAX)) as u32)
                .to_any_or_internal_err()
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

            pb::message("eve.character.career.goal.HasClaimedRewardResponse")
                .set("claimed", g.claimed)
                .to_any_or_internal_err()
        }),
    );
}

/// Converts [`IskCents`] to rounded whole ISK for whole-ISK career goals (`bounties`, `market`).
pub(crate) fn isk_cents_to_whole_isk(cents: IskCents) -> u64 {
    if cents.0 <= 0 {
        0
    } else {
        ((cents.0 as u128).saturating_add(50) / 100) as u64
    }
}

/// Emits a sub-kind-qualified AIR Career Program gameplay event directly via [`PublicGatewayRouter`].
pub(crate) fn record_career_sub_kind_gateway(
    gateway: &crate::quasar_gateway::PublicGatewayRouter,
    char_id: i64,
    activity_kind: &str,
    sub_kind: Option<&str>,
    delta: u64,
) {
    if char_id <= 0 || delta == 0 {
        return;
    }
    gateway.progression.career_goals.on_career_gameplay_event(
        char_id as u32,
        CareerGameplayEvent {
            activity_kind,
            sub_kind,
            ..Default::default()
        },
        delta,
        gateway,
    );
}

/// Emits a sub-kind-qualified AIR Career Program gameplay event for `char_id`.
pub(crate) fn record_career_sub_kind(
    ctx: &crate::router::ServerContext,
    char_id: i64,
    activity_kind: &str,
    sub_kind: Option<&str>,
    delta: u64,
) {
    record_career_sub_kind_gateway(&ctx.quasar_gateway, char_id, activity_kind, sub_kind, delta);
}

/// Dispatches `market` (`spend_isk` / `earn_isk`) and `research` (`buy_blueprint`) career goals for a market fill.
pub(crate) fn record_market_fill_career_goals(
    ctx: &crate::router::ServerContext,
    buyer_id: i64,
    seller_id: i64,
    type_id: u32,
    gross: IskCents,
) {
    let whole_isk = isk_cents_to_whole_isk(gross);
    let (buyer_is_char, seller_is_char) = ctx.db.with_snapshot(|snap| {
        (
            buyer_id > 0 && snap.characters.contains_key(&buyer_id),
            seller_id > 0 && snap.characters.contains_key(&seller_id),
        )
    });
    if buyer_is_char {
        if whole_isk > 0 {
            record_career_sub_kind(ctx, buyer_id, "market", Some("spend_isk"), whole_isk);
        }
        if let Some(inv_t) = ctx.sde.types.get(&type_id).filter(|t| t.category_id == 9) {
            ctx.quasar_gateway
                .progression
                .career_goals
                .on_career_gameplay_event(
                    buyer_id as u32,
                    CareerGameplayEvent {
                        activity_kind: "research",
                        type_id: Some(type_id),
                        group_id: Some(inv_t.group_id),
                        sub_kind: Some("buy_blueprint"),
                        ..Default::default()
                    },
                    1,
                    &ctx.quasar_gateway,
                );
        }
    }
    if seller_is_char && whole_isk > 0 {
        record_career_sub_kind(ctx, seller_id, "market", Some("earn_isk"), whole_isk);
    }
}

/// Dispatches `loyalty_points` (`earn_lp` / `spend_lp`) career progression directly via [`PublicGatewayRouter`].
pub(crate) fn record_lp_career_event_gateway(
    gateway: &crate::quasar_gateway::PublicGatewayRouter,
    char_id: i64,
    is_spend: bool,
    amount: i64,
) {
    if char_id <= 0 || amount <= 0 {
        return;
    }
    record_career_sub_kind_gateway(
        gateway,
        char_id,
        "loyalty_points",
        Some(if is_spend { "spend_lp" } else { "earn_lp" }),
        amount as u64,
    );
}

/// Dispatches `loyalty_points` (`earn_lp` / `spend_lp`) career progression for `char_id`.
pub(crate) fn record_lp_career_event(
    ctx: &crate::router::ServerContext,
    char_id: i64,
    is_spend: bool,
    amount: i64,
) {
    record_lp_career_event_gateway(&ctx.quasar_gateway, char_id, is_spend, amount);
}

/// Dispatches `bounties` career progression for whole ISK earned by `char_id`.
pub(crate) fn record_bounty_career_event(
    ctx: &crate::router::ServerContext,
    char_id: i64,
    amount_isk: u64,
) {
    record_career_sub_kind(ctx, char_id, "bounties", None, amount_isk);
}

/// Dispatches `standings` threshold progression when effective standing with a corporation rises.
pub(crate) fn record_standing_career_event(
    ctx: &crate::router::ServerContext,
    char_id: i64,
    from_id: i64,
    new_val: f64,
) {
    if char_id <= 0 || from_id <= 0 || !new_val.is_finite() || new_val <= 0.0 {
        return;
    }
    let is_corp = ctx.sde.npc_corporations.contains_key(&from_id)
        || ctx.db.with_snapshot(|s| s.corporations.contains_key(&from_id));
    if !is_corp {
        return;
    }
    let bps = (new_val.clamp(0.0, 10.0) * 100.0).round() as u64;
    if bps == 0 {
        return;
    }
    ctx.quasar_gateway
        .progression
        .career_goals
        .on_career_gameplay_event(
            char_id as u32,
            CareerGameplayEvent {
                activity_kind: "standings",
                standing_bps: Some(bps),
                ..Default::default()
            },
            1,
            &ctx.quasar_gateway,
        );
}

/// Maps a Career Agent SDE `division_id` (`25..=29`) to its AIR Career Agent division key.
pub(crate) fn career_division_key(division_id: u32) -> Option<&'static str> {
    match division_id {
        25 => Some("entrepreneur"),
        26 => Some("explorer"),
        27 => Some("producer"),
        28 => Some("enforcer"),
        29 => Some("soldier_of_fortune"),
        _ => None,
    }
}

/// Extracts `(ordinal, total)` from mission titles shaped like `... (3 of 10)` or `... (2 of 5)`.
pub(crate) fn parse_career_ordinal_from_title(title: &str) -> Option<(u32, u32)> {
    let close = title.rfind(')')?;
    let open = title[..close].rfind('(')?;
    let inner = title[open + 1..close].trim();
    let (lhs, rhs) = inner.split_once(" of ").or_else(|| inner.split_once(" OF "))?;
    let ord = lhs.trim().parse::<u32>().ok()?;
    let total = rhs.trim().parse::<u32>().ok()?;
    (ord >= 1 && total >= ord).then_some((ord, total))
}

/// Dispatches all applicable mission completion career goals (`loyalty_points`, `career_missions`,
/// `epic_arc`, `level1_fw_mission`, `distribution_missions`, `mining_missions`, `combat_missions`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_mission_completion_career_goals(
    ctx: &crate::router::ServerContext,
    char_id: i64,
    agent_id: i64,
    mission_title: &str,
    is_courier: bool,
    is_mining_mission: bool,
    is_security_mission: bool,
    is_fw_agent: bool,
    epic_arc_completed: bool,
    credited_lp: i64,
) {
    if char_id <= 0 {
        return;
    }
    crate::quasar_gateway::isolate_side_effect("quasar_career_goals.mission_complete", || {
        record_lp_career_event(ctx, char_id, false, credited_lp);
        let Some(agent) = ctx.sde.agents.get(&agent_id) else {
            return;
        };
        if agent.agent_type_id == 12 {
            if let Some(div) = career_division_key(agent.division_id) {
                let ord = if let Some((k, total)) = parse_career_ordinal_from_title(mission_title) {
                    if total == 5 && matches!(div, "entrepreneur" | "producer") {
                        k.saturating_mul(2)
                    } else {
                        k
                    }
                } else {
                    let done = ctx.db.with_snapshot(|snap| {
                        snap.active_missions
                            .values()
                            .filter(|m| {
                                m.character_id == char_id
                                    && m.agent_id == agent_id
                                    && m.state == "Completed"
                            })
                            .count() as u32
                    });
                    let base = done.clamp(1, 10);
                    if matches!(div, "entrepreneur" | "producer") && base <= 5 {
                        base.saturating_mul(2)
                    } else {
                        base
                    }
                };
                ctx.quasar_gateway
                    .progression
                    .career_goals
                    .on_career_gameplay_event(
                        char_id as u32,
                        CareerGameplayEvent {
                            activity_kind: "career_missions",
                            mission_division: Some(div),
                            mission_ordinal: Some(ord),
                            ..Default::default()
                        },
                        1,
                        &ctx.quasar_gateway,
                    );
            }
            return;
        }
        if agent.agent_type_id == 10 {
            record_career_sub_kind(ctx, char_id, "epic_arc", Some("epic_arc_mission"), 1);
            if epic_arc_completed {
                record_career_sub_kind(ctx, char_id, "epic_arc", Some("epic_arc_complete"), 1);
            }
            return;
        }
        if is_fw_agent {
            if agent.level == 1 {
                record_career_sub_kind(
                    ctx,
                    char_id,
                    "faction_warfare",
                    Some("level1_fw_mission"),
                    1,
                );
            }
            return;
        }
        if is_courier || agent.division_id == 22 {
            record_career_sub_kind(ctx, char_id, "distribution_missions", None, 1);
        } else if is_mining_mission || agent.division_id == 23 {
            record_career_sub_kind(ctx, char_id, "mining_missions", None, 1);
        } else if agent.level == 1 && (agent.division_id == 24 || is_security_mission) {
            record_career_sub_kind(ctx, char_id, "combat_missions", None, 1);
        }
    });
}

/// Returns `true` when `target_id` is another capsuleer's non-pod ship (excluding NPCs, drones, pods, structures, and self).
pub(crate) fn is_capsuleer_ship_ball_target(
    db: &WorldRepository,
    sde: &eve_sde::SdeRegistry,
    target_id: i64,
    actor_char_id: i64,
) -> bool {
    if target_id <= 0 || actor_char_id <= 0 {
        return false;
    }
    db.with_snapshot(|snap| {
        let Some(item) = snap.items.get(&target_id) else {
            return false;
        };
        if item.owner_id <= 0
            || item.owner_id == actor_char_id
            || !snap.characters.contains_key(&item.owner_id)
        {
            return false;
        }
        sde.types
            .get(&item.type_id)
            .is_some_and(|t| t.category_id == 6 && item.type_id != 670)
    })
}

/// Maps `ewar_jamming_type` string labels to AIR Career Program `ewar` `sub_kind` discriminators.
pub(crate) fn ewar_sub_kind_from_jam_type(jam_type: &str) -> Option<&'static str> {
    match jam_type {
        "webify" => Some("webify"),
        "ewTargetPaint" => Some("target_paint"),
        "warpScramblerMWD" => Some("warp_scramble"),
        "warpScrambler" | "focusedWarpScrambler" => Some("warp_disrupt"),
        "ewTrackingDisrupt" | "ewGuidanceDisrupt" => Some("weapon_disrupt"),
        _ => None,
    }
}

/// Records `ewar` career seconds when targeting another capsuleer's ship.
pub(crate) fn record_ewar_career_seconds(
    ctx: &crate::router::ServerContext,
    actor_char_id: i64,
    target_id: i64,
    jam_type: &str,
    duration_s: f64,
) {
    if duration_s <= 0.0 || !duration_s.is_finite() {
        return;
    }
    let Some(sub_kind) = ewar_sub_kind_from_jam_type(jam_type) else {
        return;
    };
    if !is_capsuleer_ship_ball_target(&ctx.db, &ctx.sde, target_id, actor_char_id) {
        return;
    }
    let secs = duration_s.round().max(1.0) as u64;
    record_career_sub_kind(ctx, actor_char_id, "ewar", Some(sub_kind), secs);
}

/// Records `cap_warfare` (`nosferatu` / `neutralizer`) career GJ against another ship/NPC.
pub(crate) fn record_cap_warfare_career(
    ctx: &crate::router::ServerContext,
    actor_char_id: i64,
    source_ship_id: i64,
    target_id: i64,
    sub_kind: &str,
    gj_amount: f64,
) {
    if actor_char_id <= 0
        || target_id <= 0
        || target_id == source_ship_id
        || !gj_amount.is_finite()
        || gj_amount <= 0.0
    {
        return;
    }
    let gj = gj_amount.round() as u64;
    if gj > 0 {
        record_career_sub_kind(ctx, actor_char_id, "cap_warfare", Some(sub_kind), gj);
    }
}

/// Records `remote_repair` (`shield_boost` / `armor_repair`) career HP repaired on another capsuleer's ship.
pub(crate) fn record_remote_repair_career(
    ctx: &crate::router::ServerContext,
    actor_char_id: i64,
    target_id: i64,
    sub_kind: &str,
    hp_amount: f64,
) {
    if !hp_amount.is_finite()
        || hp_amount <= 0.0
        || !is_capsuleer_ship_ball_target(&ctx.db, &ctx.sde, target_id, actor_char_id)
    {
        return;
    }
    let hp = hp_amount.round() as u64;
    if hp > 0 {
        record_career_sub_kind(ctx, actor_char_id, "remote_repair", Some(sub_kind), hp);
    }
}

/// Returns `true` when an exploration/dungeon site category is a combat site for Career Program tracking.
pub(crate) fn is_combat_site_category(site_category: &str) -> bool {
    matches!(
        site_category,
        "Combat"
            | "CombatAnomaly"
            | "CombatSignature"
            | "CombatSite"
            | "Combat Site"
            | "DED"
            | "WormholeCombat"
            | "Escalation"
    )
}

/// Dispatches `gas_sites` or `combat_sites` (`enter_combat_site`) career progression on anomaly/signature entry.
pub(crate) fn record_site_entry_career_goals(
    ctx: &crate::router::ServerContext,
    char_id: i64,
    site_category: &str,
) {
    if matches!(site_category, "Gas" | "Gas Site" | "GasSite") {
        record_career_sub_kind(ctx, char_id, "gas_sites", None, 1);
    } else if is_combat_site_category(site_category) {
        record_career_sub_kind(ctx, char_id, "combat_sites", Some("enter_combat_site"), 1);
    }
}

/// Dispatches `combat_sites` (`complete_combat_site`) career progression when a combat site finishes.
pub(crate) fn record_combat_site_completion_gateway(
    gateway: &crate::quasar_gateway::PublicGatewayRouter,
    char_id: i64,
    site_category: &str,
) {
    if is_combat_site_category(site_category) {
        record_career_sub_kind_gateway(
            gateway,
            char_id,
            "combat_sites",
            Some("complete_combat_site"),
            1,
        );
    }
}

#[cfg(test)] #[path = "quasar_career_goals_tests.rs"]
mod quasar_career_goals_tests;

