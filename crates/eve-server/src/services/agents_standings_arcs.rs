//! `agents_standings_arcs` — Submodule of `agents_missions` (`E-5`).
//!
//! Implements:
//! - Effective standing calculation (`Connections`, `Diplomacy`, `Criminal Connections`, `Social`)
//! - Derived faction standings, opposing faction losses, and bipolar Triglavian/EDENCOM standings
//! - `OnStandingSet` and `OnStandingsModified` client notifications
//! - `standingMgr` (`GetCorpStandings`, `GetNPCNPCStandings`, `GetStandingCompositions`)
//! - `missionTrackerMgr` (`GetAllMissionObjectives`, `GetMissionInfoItems`, `IsInActiveDungeonID`, `UpdateAllMissions`)
//! - `epicArcStatus` (`AgentHasEpicMissionsForCharacter`) and `agentMgr` Epic Arc / Career / Plausible / Disabled mission RPCs
//! - R&D agent research (`DoAction` 12/13/14) and Locator agent character location (`DoAction` 15/16/17)
//! - Epic Arc / COSMOS / Data Center choice branching (`DoAction` 18/19)
//! - `keeper.ActivateAccelerationGate` (including Anomic Burner ship restrictions) and AIR Career / NPE handlers


#![forbid(unsafe_code)]

use crate::formulas::{apply_standing_decrease, apply_standing_increase};
use crate::router::notify_registry::{OnMissionsUpdated, OnStandingSet, OnStandingsModified};
use crate::router::{NotificationTarget, ServerContext};
use eve_db::{IskCents, StandingTransactionRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::{BTreeMap, HashMap, HashSet};

const SKILL_SOCIAL: TypeId = 3355;
const SKILL_DIPLOMACY: TypeId = 3357;
const SKILL_CONNECTIONS: TypeId = 3359;
const SKILL_CRIMINAL_CONNECTIONS: TypeId = 3361;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct MissionTimingAndCargoMeta {
    pub mission_id: u32,
    pub agent_id: i64,
    pub offered_at_unix: u64,
    pub offer_expiry_unix: u64,
    pub accepted_at_unix: Option<u64>,
    pub accept_expiry_unix: Option<u64>,
    pub bonus_expiry_unix: Option<u64>,
    pub completed_at_unix: Option<u64>,
    pub required_delivery_type_id: Option<TypeId>,
    pub required_delivery_qty: i32,
    pub granted_courier_item_id: Option<i64>,
    #[serde(default)]
    pub granted_courier_qty: i32,
    pub is_anomic_burner: bool,
    pub chosen_branch: Option<String>,
    #[serde(default)]
    pub destination_station_id: Option<i64>,
    #[serde(default)]
    pub origin_station_id: Option<i64>,
    #[serde(default)]
    pub offered_level: Option<u8>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RndResearchEntry {
    pub agent_id: i64,
    pub research_type_id: TypeId,
    pub points_per_day: f64,
    pub initial_points: f64,
    pub started_unix: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EpicArcStateEntry {
    pub epic_arc_id: u32,
    pub agent_id: i64,
    pub faction_id: i64,
    pub chapter: u32,
    pub completed_missions: Vec<u32>,
    pub chosen_branch: Option<String>,
    pub completed_at_unix: Option<u64>,
    pub cooldown_until_unix: Option<u64>,
}

fn mission_meta_note_id(char_id: i64, mission_id: u32) -> i64 {
    let mixed = ((char_id.unsigned_abs() & 0xFFFF_FFFF) << 20) ^ (u64::from(mission_id) & 0xFFFFF);
    -((mixed as i64).max(1) + 5_000_000_000_000)
}

pub fn load_mission_meta(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    mission_id: u32,
) -> Option<MissionTimingAndCargoMeta> {
    snap.owner_notes
        .get(&mission_meta_note_id(char_id, mission_id))
        .and_then(|n| serde_json::from_str(&n.note).ok())
}

pub fn save_mission_meta(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    meta: &MissionTimingAndCargoMeta,
) {
    let nid = mission_meta_note_id(char_id, meta.mission_id);
    if let Ok(note) = serde_json::to_string(meta) {
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: -5,
                item_id: meta.mission_id as i64,
                label: format!("mission_meta:{char_id}:{}", meta.mission_id),
                note,
                timestamp_unix: meta.offered_at_unix,
            },
        );
    }
}

fn rnd_research_note_id(char_id: i64) -> i64 {
    -(5_100_000_000_000_i64.saturating_add(char_id.abs()))
}

pub fn load_rnd_research_entries(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) -> Vec<RndResearchEntry> {
    snap.owner_notes
        .get(&rnd_research_note_id(char_id))
        .and_then(|n| serde_json::from_str(&n.note).ok())
        .unwrap_or_default()
}

pub fn save_rnd_research_entries(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    entries: &[RndResearchEntry],
) {
    let nid = rnd_research_note_id(char_id);
    if let Ok(note) = serde_json::to_string(entries) {
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: -5,
                item_id: char_id,
                label: format!("rnd_research:{char_id}"),
                note,
                timestamp_unix: 0,
            },
        );
    }
    snap.char_research_agents.insert(
        char_id,
        entries
            .iter()
            .map(|e| eve_db::CharacterResearchAgentRecord {
                agent_id: e.agent_id,
                skill_type_id: e.research_type_id as u32,
                started_at_unix: e.started_unix,
                points_per_day: e.points_per_day,
                remainder_points: e.initial_points,
            })
            .collect(),
    );
}

fn epic_arc_note_id(char_id: i64) -> i64 {
    -(5_200_000_000_000_i64.saturating_add(char_id.abs()))
}

pub fn load_epic_arc_states(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) -> Vec<EpicArcStateEntry> {
    snap.owner_notes
        .get(&epic_arc_note_id(char_id))
        .and_then(|n| serde_json::from_str(&n.note).ok())
        .unwrap_or_default()
}

pub fn save_epic_arc_states(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    entries: &[EpicArcStateEntry],
) {
    let nid = epic_arc_note_id(char_id);
    if let Ok(note) = serde_json::to_string(entries) {
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: -5,
                item_id: char_id,
                label: format!("epic_arcs:{char_id}"),
                note,
                timestamp_unix: 0,
            },
        );
    }
}

fn locator_cooldown_note_id(char_id: i64, agent_id: i64) -> i64 {
    let mixed = ((char_id.unsigned_abs() & 0xFFFF_FFFF) << 20) ^ (agent_id.unsigned_abs() & 0xFFFFF);
    -((mixed as i64).max(1) + 5_300_000_000_000)
}

pub fn resolve_epic_arc_id_for_agent(ctx: &ServerContext, agent_id: i64) -> (u32, i64) {
    let corp_id = ctx.sde.agents.get(&agent_id).map_or(1_000_130, |a| a.corporation_id);
    let faction_id = ctx.sde.npc_corporations
        .get(&corp_id)
        .and_then(|c| c.faction_id)
        .map_or(500_003, |f| f as i64);
    let arc_id = match faction_id {
        500_001 => 2,
        500_002 => 3,
        500_003 => 4,
        500_004 => 5,
        500_010 => 6,
        500_011 => 7,
        _ => 1,
    };
    (arc_id, faction_id)
}

/// Maps V24 chapter-start `missionID -> chapterTitleMessageID` from `missions.fsdbinary`
/// across all 9 active V24 Epic Arcs (`29, 40, 48, 52, 53, 55, 56, 128, 132`) so
/// `EpicArcController._ConnectAllNodes` and `GetCurrentChapterID` resolve every chapter cleanly.
const EPIC_ARC_CHAPTER_TITLES: &[(u32, i64)] = &[
    // Arc 29: The Blood-Stained Stars
    (14117, 140784), // Quality of Mercy
    (14127, 140792), // Automaton Impediment
    (14132, 140797), // Shadow Puppets
    (14138, 140804), // Queens and Drones
    (14391, 140813), // Shifting Foundations
    (14399, 140820), // A Breach of Trust
    (14413, 140828), // Closing In
    // Arc 40: Penumbra
    (14603, 141721), // Blackness Rising
    (14616, 141720), // Blackness Rising
    (14849, 141900), // Proof of Intent
    (14969, 142236), // The Bottom Line
    (14970, 142235), // The Bottom Line
    // Arc 48: Right to Rule
    (14623, 141388), // Interference
    (14633, 141425), // Spiderweb Politics
    (14843, 141922), // The Old Guard
    (14906, 141962), // Alike Minds
    // Arc 52: Syndication
    (14746, 142238), // Impetus
    (14889, 141793), // Eagle Grip
    (14890, 142239), // Scoping the Scene
    (14942, 142240), // Inertia
    // Arc 53: Wildfire
    (14851, 141611), // The Passage
    (14934, 142043), // Songs of the Past
    (14961, 142124), // Revelation
    // Arc 55: Smash and Grab
    (15018, 142536), // Chapter 1 - Probationary
    (15055, 142648), // Chapter 2 - For Fun and Profit
    (15059, 142727), // Chapter 3 - Internal Security
    (15143, 143100), // Chapter 1 - Probationary
    // Arc 56: Angel Sound
    (15029, 142820), // Rapture
    (15033, 142822), // Utopia
    (15034, 142821), // Heaven
    (15045, 142824), // Paradise
    (15046, 142823), // Paradise
    // Arc 128: Fractured Legacy
    (17130, 1017644), // The Missing Pilot
    (17131, 1017641), // Pioneering Spirit
    (17132, 1017642), // Shattered Catalyst
    // Arc 132: Visions of Greatness
    (17183, 1039696), // Golgothan Fields
    (17184, 1039706), // Caldari Prime Breakout
    (17185, 1039708), // Intaki Liberation
    (17186, 1039710), // Battle of Pator
    (17187, 1040027), // Travel to the Amarr
    (17188, 1040276), // Holographic Reenactment
    (17189, 1040285), // Travel to the Gallente
    (17190, 1040294), // Travel to the Minmatar
];

/// Maps V24 Epic Arc `missionID -> (epicArcID, nameID, inProgressMsgID, completedMsgID)` across
/// all 21 arcs (`29, 40, 48, 52, 53, 55, 56, 64, 67..=77, 128, 132`) from `epicarcs.fsdbinary`
/// and `missions.fsdbinary`.
const EPIC_ARC_MISSION_SPECS: &[(u32, u32, i64, i64, i64)] = &[
    // Arc 29: The Blood-Stained Stars
    (14117, 29, 57289, 140983, 140853), (14118, 29, 57269, 141034, 141029), (14119, 29, 57287, 140857, 140984),
    (14120, 29, 57266, 141099, 141030), (14121, 29, 57267, 141031, 141032), (14122, 29, 57262, 141013, 141014),
    (14123, 29, 57254, 140858, 140859), (14124, 29, 57275, 140864, 140865), (14125, 29, 57265, 141098, 141097),
    (14126, 29, 57255, 141036, 141037), (14127, 29, 57268, 140868, 141103), (14128, 29, 57234, 140869, 140870),
    (14129, 29, 58617, 140871, 140872), (14130, 29, 58618, 141094, 141095), (14131, 29, 57256, 140873, 140874),
    (14132, 29, 57235, 140875, 140876), (14133, 29, 57237, 140879, 140880), (14134, 29, 57236, 140877, 140878),
    (14135, 29, 57238, 140881, 140882), (14136, 29, 57257, 140885, 140886), (14137, 29, 58619, 140883, 140884),
    (14138, 29, 57239, 140887, 140888), (14139, 29, 57282, 140902, 140903), (14140, 29, 57252, 140895, 140896),
    (14141, 29, 57240, 140900, 140901), (14142, 29, 57242, 140908, 140909), (14386, 29, 57295, 140904, 140905),
    (14387, 29, 57241, 140906, 140907), (14388, 29, 57243, 140910, 140911), (14389, 29, 57244, 140912, 140913),
    (14390, 29, 58620, 140915, 140916), (14391, 29, 57245, 140918, 140919), (14392, 29, 57258, 140920, 140921),
    (14393, 29, 57246, 140922, 140923), (14394, 29, 57247, 140924, 140925), (14396, 29, 57248, 140928, 140929),
    (14397, 29, 57249, 140930, 140931), (14398, 29, 57260, 140932, 140933), (14399, 29, 57250, 140934, 140935),
    (14401, 29, 57293, 140936, 140937), (14403, 29, 57283, 140938, 140939), (14404, 29, 57284, 140940, 140941),
    (14405, 29, 57261, 140942, 140943), (14406, 29, 57291, 140944, 140945), (14407, 29, 57292, 140946, 140947),
    (14413, 29, 57290, 140948, 140949), (14414, 29, 57271, 140950, 140951), (14415, 29, 57270, 140952, 140953),
    (14416, 29, 57251, 140954, 140955), (14422, 29, 57259, 140926, 140927), (14442, 29, 57253, 140956, 140957),
    (14443, 29, 57294, 140958, 141135), (14444, 29, 57277, 140959, 141136), (14445, 29, 57278, 140960, 141137),
    (14446, 29, 57279, 140961, 141138), (14451, 29, 57285, 140917, 141236), (14453, 29, 57272, 141126, 141127),
    (14454, 29, 57273, 141129, 141128), (14455, 29, 57274, 141130, 141228), (14456, 29, 57276, 141133, 141134),
    (14467, 29, 57281, 141139, 141140), (14468, 29, 57280, 141141, 141241), (14469, 29, 57296, 141142, 141243),
    (14470, 29, 57297, 141143, 141144),
    // Arc 40: Penumbra
    (14490, 40, 57299, 141287, 141904), (14492, 40, 57549, 142296, 142295), (14511, 40, 57552, 141493, 141906),
    (14548, 40, 57551, 142300, 142299), (14552, 40, 58615, 142291, 141914), (14553, 40, 57550, 142288, 141916),
    (14558, 40, 57307, 142292, 142293), (14559, 40, 57308, 142289, 142290), (14576, 40, 57310, 141545, 141907),
    (14597, 40, 57311, 141351, 141910), (14599, 40, 57312, 141355, 141909), (14601, 40, 57313, 141690, 141905),
    (14602, 40, 57314, 141580, 141908), (14603, 40, 57479, 141708, 141709), (14607, 40, 57315, 141698, 141699),
    (14610, 40, 57317, 141487, 141919), (14612, 40, 57537, 141448, 141918), (14613, 40, 57535, 141432, 141917),
    (14616, 40, 57480, 141718, 141719), (14619, 40, 57319, 141695, 141696), (14827, 40, 57536, 141597, 141915),
    (14833, 40, 57468, 142301, 142302), (14847, 40, 57482, 141730, 141731), (14848, 40, 57481, 141738, 141911),
    (14849, 40, 57470, 141596, 141903), (14882, 40, 57567, 141746, 141747), (14884, 40, 57566, 141755, 141756),
    (14885, 40, 57483, 141764, 142343), (14886, 40, 57484, 141772, 142344), (14898, 40, 57496, 141912, 141913),
    (14900, 40, 57498, 141921, 141920), (14901, 40, 58613, 141850, 141851), (14903, 40, 58614, 141866, 141867),
    (14969, 40, 57528, 142197, 142198), (14970, 40, 57529, 142204, 142205), (14971, 40, 57530, 142214, 142215),
    (14972, 40, 57531, 142222, 142223), (14973, 40, 57532, 142211, 142212), (14975, 40, 57533, 142241, 142242),
    (14976, 40, 57534, 142227, 142228),
    // Arc 48: Right to Rule
    (14623, 48, 57320, 141368, 141401), (14624, 48, 57463, 141372, 141376), (14627, 48, 57321, 141407, 141408),
    (14628, 48, 57322, 141382, 141386), (14633, 48, 57478, 141677, 141424), (14645, 48, 57464, 141674, 141675),
    (14646, 48, 57465, 141455, 141454), (14647, 48, 57466, 141463, 141464), (14648, 48, 57467, 141471, 141472),
    (14838, 48, 58612, 141561, 141565), (14839, 48, 57485, 141569, 141570), (14842, 48, 57469, 141608, 141607),
    (14843, 48, 57486, 141603, 142003), (14860, 48, 57500, 141638, 141637), (14861, 48, 58611, 141931, 141930),
    (14862, 48, 57476, 141938, 141937), (14863, 48, 57477, 141947, 141946), (14864, 48, 57539, 141952, 141957),
    (14906, 48, 57506, 142304, 142306), (14912, 48, 57502, 141972, 141973), (14913, 48, 57503, 141980, 141982),
    (14914, 48, 57504, 142342, 142317), (14915, 48, 57505, 141992, 141997), (14916, 48, 57507, 143352, 143351),
    // Arc 52: Syndication
    (14746, 52, 57395, 141507, 141508), (14819, 52, 58616, 141516, 141517), (14820, 52, 57460, 141681, 141679),
    (14821, 52, 57461, 141531, 141532), (14822, 52, 57462, 141538, 141682), (14831, 52, 57501, 141556, 141557),
    (14888, 52, 57487, 141784, 141785), (14889, 52, 57488, 141791, 141792), (14890, 52, 57489, 141857, 141858),
    (14891, 52, 57490, 141800, 141801), (14892, 52, 57491, 141874, 141875), (14893, 52, 57492, 141811, 141812),
    (14894, 52, 57493, 141882, 141883), (14895, 52, 57494, 141820, 141821), (14896, 52, 57495, 141890, 141891),
    (14899, 52, 57497, 141843, 141844), (14905, 52, 57499, 141898, 141899), (14942, 52, 57511, 142046, 142047),
    (14943, 52, 57512, 142058, 142059), (14944, 52, 57513, 142066, 142067), (14945, 52, 57514, 142075, 142076),
    (14946, 52, 57515, 142287, 142286), (14947, 52, 57516, 142095, 142096), (14948, 52, 57517, 142103, 142104),
    (14950, 52, 57519, 142273, 142111), (14951, 52, 57520, 142274, 142120), (14980, 52, 57538, 142270, 142271),
    // Arc 53: Wildfire
    (14851, 53, 57471, 142325, 141620), (14852, 53, 57472, 142335, 142337), (14853, 53, 57473, 141640, 141641),
    (14854, 53, 57540, 141649, 141650), (14855, 53, 57474, 141657, 141658), (14856, 53, 57475, 141668, 141669),
    (14932, 53, 57508, 142009, 142019), (14934, 53, 57509, 142015, 142018), (14935, 53, 57510, 142028, 142313),
    (14936, 53, 57541, 142034, 142035), (14937, 53, 57542, 142041, 142042), (14960, 53, 57543, 142138, 142139),
    (14961, 53, 57521, 142136, 142137), (14962, 53, 57522, 142144, 142145), (14963, 53, 57523, 142151, 142152),
    (14965, 53, 57524, 142164, 142165), (14966, 53, 57525, 142169, 142170), (14967, 53, 57526, 142183, 142184),
    (14968, 53, 57527, 142177, 142178),
    // Arc 55: Smash and Grab
    (15015, 55, 57560, 142521, 142522), (15017, 55, 57561, 142527, 142528), (15018, 55, 57562, 142537, 142538),
    (15019, 55, 57563, 142539, 142540), (15020, 55, 57564, 142545, 142546), (15021, 55, 57565, 142547, 142552),
    (15026, 55, 57568, 142556, 142561), (15053, 55, 57586, 142664, 142669), (15055, 55, 57587, 142646, 142652),
    (15056, 55, 57588, 143252, 143253), (15057, 55, 57597, 142690, 142702), (15059, 55, 57589, 142726, 142728),
    (15060, 55, 57590, 142730, 142735), (15061, 55, 57591, 142745, 142750), (15062, 55, 57592, 142736, 142741),
    (15063, 55, 57593, 142769, 142774), (15064, 55, 57594, 142777, 142782), (15065, 55, 57595, 142753, 142758),
    (15066, 55, 57596, 142761, 142766), (15143, 55, 57607, 143097, 143101),
    // Arc 56: Angel Sound
    (15028, 56, 57608, 142581, 142582), (15029, 56, 57611, 142591, 142592), (15030, 56, 57569, 142840, 142841),
    (15031, 56, 57570, 142832, 142833), (15032, 56, 57571, 142818, 142819), (15033, 56, 57572, 142597, 142598),
    (15034, 56, 57573, 142599, 142600), (15035, 56, 57574, 142615, 142616), (15036, 56, 57575, 142704, 142705),
    (15037, 56, 57576, 142688, 142689), (15038, 56, 57577, 142624, 142625), (15039, 56, 57578, 142632, 142633),
    (15040, 56, 57620, 142676, 142677), (15041, 56, 57579, 142662, 142659), (15042, 56, 57580, 142641, 142642),
    (15043, 56, 57581, 142785, 142786), (15044, 56, 57582, 142789, 142787), (15045, 56, 57583, 142796, 142795),
    (15046, 56, 57584, 142804, 142803), (15047, 56, 57585, 142809, 142810), (15058, 56, 57609, 142715, 142716),
    (15144, 56, 57610, 143139, 143140),
    // Arc 64: Pilot Certification Course - Emrayur
    (15565, 64, 58367, 144600, 144590), (15566, 64, 58359, 144592, 144591), (15568, 64, 58365, 144603, 144602),
    (15621, 64, 58556, 144683, 144682),
    // Arc 67: Pilot Certification Course - Chaven
    (15622, 67, 58557, 144685, 144688), (15623, 67, 58558, 144691, 144694), (15624, 67, 58559, 144697, 144700),
    (15625, 67, 58560, 144703, 144706),
    // Arc 68: Pilot Certification Course - Sehmy
    (15626, 68, 58561, 144710, 144713), (15627, 68, 58562, 144715, 144718), (15628, 68, 58563, 144722, 144725),
    (15629, 68, 58564, 144728, 144731),
    // Arc 69: Pilot Certification Course - Bourynes
    (15630, 69, 58565, 144735, 144738), (15631, 69, 58566, 144741, 144744), (15632, 69, 58567, 144747, 144750),
    (15633, 69, 58568, 144753, 144756),
    // Arc 70: Pilot Certification Course - Duripant
    (15634, 70, 58569, 144760, 144763), (15635, 70, 58570, 144766, 144769), (15636, 70, 58571, 144772, 144775),
    (15637, 70, 58572, 144778, 144781),
    // Arc 71: Pilot Certification Course - Cistuvaert
    (15638, 71, 58573, 144785, 144788), (15639, 71, 58574, 144790, 144793), (15640, 71, 58575, 144796, 144799),
    (15641, 71, 58576, 144802, 144805),
    // Arc 72: Pilot Certification Course - Hulm
    (15642, 72, 58577, 144808, 144811), (15643, 72, 58578, 144814, 144817), (15644, 72, 58579, 144820, 144823),
    (15645, 72, 58580, 144826, 144829),
    // Arc 73: Pilot Certification Course - Ammold
    (15646, 73, 58581, 144833, 144836), (15647, 73, 58582, 145211, 145210), (15648, 73, 58583, 144844, 144847),
    (15649, 73, 58584, 144850, 144853),
    // Arc 74: Pilot Certification Course - Ryddinjorn
    (15650, 74, 58585, 144857, 144860), (15651, 74, 58586, 144863, 144866), (15652, 74, 58587, 144869, 144872),
    (15653, 74, 58588, 144875, 144878),
    // Arc 75: Pilot Certification Course - Kisogo
    (15654, 75, 58589, 144882, 144885), (15655, 75, 58590, 144888, 144891), (15656, 75, 58591, 144894, 144897),
    (15657, 75, 58592, 144900, 144903),
    // Arc 76: Pilot Certification Course - Amsen
    (15658, 76, 58593, 144907, 144910), (15659, 76, 58594, 144913, 144916), (15660, 76, 58595, 144919, 144922),
    (15661, 76, 58596, 144925, 144928),
    // Arc 77: Pilot Certification Course - Todaki
    (15662, 77, 58597, 144932, 144935), (15663, 77, 58598, 144938, 144941), (15664, 77, 58599, 144944, 144947),
    (15665, 77, 58600, 144950, 144953),
    // Arc 128: Fractured Legacy
    (17130, 128, 1016269, 1016272, 1016271), (17131, 128, 1016439, 1016441, 1016440), (17132, 128, 1016449, 1016451, 1016450),
    // Arc 132: Visions of Greatness
    (17183, 132, 1039695, 1039730, 1039729), (17184, 132, 1039705, 1039746, 1039745), (17185, 132, 1039707, 1039738, 1039737),
    (17186, 132, 1039709, 1039755, 1039754), (17187, 132, 1040026, 1040029, 1040028), (17188, 132, 1040275, 1040278, 1040277),
    (17189, 132, 1040274, 1040287, 1040286), (17190, 132, 1040273, 1040296, 1040295),
];

fn epic_arc_mission_spec(mission_id: u32) -> Option<(u32, i64)> {
    EPIC_ARC_MISSION_SPECS
        .iter()
        .find(|&&(mid, _, _, _, _)| mid == mission_id)
        .map(|&(_, arc_id, name_id, _, _)| (arc_id, name_id))
}

fn is_v24_epic_arc_id(arc_id: u32) -> bool {
    matches!(
        arc_id,
        29 | 40 | 48 | 52 | 53 | 55 | 56 | 64 | 67..=77 | 128 | 132
    )
}

/// Coalesces legacy faction-mapped `epic_arc_id` rows (`1..=7`) written during mission
/// completion onto the V24 `epicArcID` whenever `completed_missions` holds V24 mission IDs.
fn resolve_entry_epic_arc_id(entry: &EpicArcStateEntry) -> u32 {
    if is_v24_epic_arc_id(entry.epic_arc_id) {
        return entry.epic_arc_id;
    }
    entry
        .completed_missions
        .iter()
        .find_map(|&mid| epic_arc_mission_spec(mid).map(|(arc_id, _)| arc_id))
        .unwrap_or(entry.epic_arc_id)
}

fn is_epic_agent(ctx: &ServerContext, agent_id: i64) -> bool {
    ctx.sde
        .agents
        .get(&agent_id)
        .is_some_and(|ag| ag.agent_type_id == 10)
}

fn active_mission_matches_arc(
    ctx: &ServerContext,
    entry_arc_id: u32,
    entry_agent_id: i64,
    mission: &eve_db::ActiveMissionRecord,
) -> bool {
    if let Some((spec_arc_id, _)) = epic_arc_mission_spec(mission.mission_id) {
        return spec_arc_id == entry_arc_id;
    }
    if mission.agent_id == entry_agent_id {
        return true;
    }
    is_epic_agent(ctx, mission.agent_id)
        && resolve_epic_arc_id_for_agent(ctx, mission.agent_id).0 == entry_arc_id
}

fn unix_to_filetime_rep(ts_unix: u64) -> PyRep {
    let clamped = i64::try_from(ts_unix).unwrap_or(i64::MAX);
    let filetime = clamped
        .saturating_add(11_644_473_600)
        .saturating_mul(10_000_000);
    PyRep::Integer(filetime)
}

struct EpicArcMissionFields<'a> {
    mission_id: u32,
    accepted_date: PyRep,
    completed_date: PyRep,
    epic_arc_id: u32,
    agent_id: i64,
    faction_id: i64,
    chapter: u32,
    state: &'a str,
    on_cooldown: bool,
    chosen_branch: &'a PyRep,
    cooldown_until: &'a PyRep,
}

fn build_epic_arc_mission_entry(f: EpicArcMissionFields<'_>) -> (PyRep, PyRep) {
    let name_id = if f.mission_id == 0 {
        0
    } else {
        epic_arc_mission_spec(f.mission_id).map_or(i64::from(f.mission_id), |(_, nid)| nid)
    };
    (
        PyRep::Integer(i64::from(f.mission_id)),
        PyRep::keyval([
            ("acceptedDate", f.accepted_date),
            ("completedDate", f.completed_date),
            ("quitDate", PyRep::None),
            ("nameID", PyRep::Integer(name_id)),
            ("epicArcID", PyRep::Integer(i64::from(f.epic_arc_id))),
            ("agentID", PyRep::Integer(f.agent_id)),
            ("factionID", PyRep::Integer(f.faction_id)),
            ("chapter", PyRep::Integer(i64::from(f.chapter))),
            ("state", PyRep::String(f.state.to_owned())),
            ("onCooldown", PyRep::Bool(f.on_cooldown)),
            ("chosenBranch", f.chosen_branch.clone()),
            ("cooldownUntilUnix", f.cooldown_until.clone()),
        ]),
    )
}

fn active_epic_arc_accepted_date(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    mission_id: u32,
    state: &str,
    now_unix: u64,
) -> PyRep {
    if state == "Offered" {
        return PyRep::None;
    }
    let accepted_unix = load_mission_meta(snap, char_id, mission_id)
        .and_then(|meta| meta.accepted_at_unix)
        .unwrap_or(now_unix);
    unix_to_filetime_rep(accepted_unix)
}

struct EpicArcEmitCtx<'a> {
    arc_id: u32,
    faction_id: i64,
    chapter: u32,
    state: &'a str,
    on_cooldown: bool,
    chosen_branch: &'a PyRep,
    cooldown_until: &'a PyRep,
    now_unix: u64,
}

fn completed_epic_arc_mission_entry(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    mission_id: u32,
    entry: &EpicArcStateEntry,
    emit: &EpicArcEmitCtx<'_>,
) -> (PyRep, PyRep) {
    let meta = load_mission_meta(snap, char_id, mission_id);
    let accepted_unix = meta
        .as_ref()
        .and_then(|mt| mt.accepted_at_unix)
        .or(entry.completed_at_unix)
        .unwrap_or(emit.now_unix);
    let completed_unix = meta
        .as_ref()
        .and_then(|mt| mt.completed_at_unix)
        .or(entry.completed_at_unix)
        .unwrap_or(emit.now_unix);
    build_epic_arc_mission_entry(EpicArcMissionFields {
        mission_id,
        accepted_date: unix_to_filetime_rep(accepted_unix),
        completed_date: unix_to_filetime_rep(completed_unix),
        epic_arc_id: emit.arc_id,
        agent_id: entry.agent_id,
        faction_id: emit.faction_id,
        chapter: emit.chapter,
        state: emit.state,
        on_cooldown: emit.on_cooldown,
        chosen_branch: emit.chosen_branch,
        cooldown_until: emit.cooldown_until,
    })
}

fn active_epic_arc_mission_entry(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    m: &eve_db::ActiveMissionRecord,
    emit: &EpicArcEmitCtx<'_>,
) -> (PyRep, PyRep) {
    build_epic_arc_mission_entry(EpicArcMissionFields {
        mission_id: m.mission_id,
        accepted_date: active_epic_arc_accepted_date(
            snap,
            char_id,
            m.mission_id,
            &m.state,
            emit.now_unix,
        ),
        completed_date: PyRep::None,
        epic_arc_id: emit.arc_id,
        agent_id: m.agent_id,
        faction_id: emit.faction_id,
        chapter: emit.chapter,
        state: emit.state,
        on_cooldown: emit.on_cooldown,
        chosen_branch: emit.chosen_branch,
        cooldown_until: emit.cooldown_until,
    })
}

struct EpicArcStatusCollector<'a> {
    ctx: &'a ServerContext,
    snap: &'a eve_db::WorldStateSnapshot,
    char_id: i64,
    now_unix: u64,
    active_missions: Vec<&'a eve_db::ActiveMissionRecord>,
    handled_mission_ids: HashSet<u32>,
    handled_active_ids: HashSet<u32>,
    by_arc: BTreeMap<u32, Vec<(PyRep, PyRep)>>,
}

impl<'a> EpicArcStatusCollector<'a> {
    fn new(
        ctx: &'a ServerContext,
        snap: &'a eve_db::WorldStateSnapshot,
        char_id: i64,
        now_unix: u64,
    ) -> Self {
        let mut active_missions: Vec<&'a eve_db::ActiveMissionRecord> = snap
            .active_missions
            .values()
            .filter(|m| {
                m.character_id == char_id && matches!(m.state.as_str(), "Offered" | "Accepted")
            })
            .collect();
        active_missions.sort_by_key(|m| m.mission_id);
        Self {
            ctx,
            snap,
            char_id,
            now_unix,
            active_missions,
            handled_mission_ids: HashSet::new(),
            handled_active_ids: HashSet::new(),
            by_arc: BTreeMap::new(),
        }
    }

    fn push_recorded_entry(&mut self, a: &EpicArcStateEntry) {
        let arc_id = resolve_entry_epic_arc_id(a);
        let on_cd = a
            .cooldown_until_unix
            .is_some_and(|until| self.now_unix < until);
        let arc_active: Vec<&'a eve_db::ActiveMissionRecord> = self
            .active_missions
            .iter()
            .copied()
            .filter(|m| {
                !self.handled_active_ids.contains(&m.mission_id)
                    && active_mission_matches_arc(self.ctx, arc_id, a.agent_id, m)
            })
            .collect();
        let active_ids: HashSet<u32> = arc_active.iter().map(|m| m.mission_id).collect();
        let is_restarted = a.completed_at_unix.is_some() && !on_cd && !arc_active.is_empty();
        let state_str = if !arc_active.is_empty() || a.completed_at_unix.is_none() {
            "Active"
        } else if on_cd {
            "Cooldown"
        } else {
            "Completed"
        };
        let effective_on_cd = on_cd && arc_active.is_empty();
        let branch_rep = a
            .chosen_branch
            .clone()
            .map(PyRep::String)
            .unwrap_or(PyRep::None);
        let cd_rep = if is_restarted {
            PyRep::None
        } else {
            a.cooldown_until_unix
                .map(|u| PyRep::Integer(i64::try_from(u).unwrap_or(i64::MAX)))
                .unwrap_or(PyRep::None)
        };
        let emit = EpicArcEmitCtx {
            arc_id,
            faction_id: a.faction_id,
            chapter: a.chapter,
            state: state_str,
            on_cooldown: effective_on_cd,
            chosen_branch: &branch_rep,
            cooldown_until: &cd_rep,
            now_unix: self.now_unix,
        };

        let missions_dict = self.by_arc.entry(arc_id).or_default();
        if !is_restarted {
            for &m in &a.completed_missions {
                if active_ids.contains(&m) || !self.handled_mission_ids.insert(m) {
                    continue;
                }
                missions_dict.push(completed_epic_arc_mission_entry(
                    self.snap,
                    self.char_id,
                    m,
                    a,
                    &emit,
                ));
            }
        }
        for m in arc_active {
            self.handled_active_ids.insert(m.mission_id);
            self.handled_mission_ids.insert(m.mission_id);
            missions_dict.push(active_epic_arc_mission_entry(
                self.snap,
                self.char_id,
                m,
                &emit,
            ));
        }
        if missions_dict.is_empty() {
            let completed_date = if is_restarted {
                PyRep::None
            } else {
                a.completed_at_unix
                    .map(unix_to_filetime_rep)
                    .unwrap_or(PyRep::None)
            };
            missions_dict.push(build_epic_arc_mission_entry(EpicArcMissionFields {
                mission_id: 0,
                accepted_date: unix_to_filetime_rep(self.now_unix),
                completed_date,
                epic_arc_id: arc_id,
                agent_id: a.agent_id,
                faction_id: a.faction_id,
                chapter: a.chapter,
                state: state_str,
                on_cooldown: effective_on_cd,
                chosen_branch: &branch_rep,
                cooldown_until: &cd_rep,
            }));
        }
    }

    fn finish(mut self) -> Vec<(PyRep, PyRep)> {
        for m in self.active_missions {
            if self.handled_active_ids.contains(&m.mission_id)
                || self.handled_mission_ids.contains(&m.mission_id)
            {
                continue;
            }
            let spec = epic_arc_mission_spec(m.mission_id);
            if spec.is_none() && !is_epic_agent(self.ctx, m.agent_id) {
                continue;
            }
            let (resolved_arc_id, agent_faction_id) =
                resolve_epic_arc_id_for_agent(self.ctx, m.agent_id);
            let arc_id = spec.map_or(resolved_arc_id, |(aid, _)| aid);
            let faction_id = self
                .ctx
                .sde
                .epic_arcs
                .get(&arc_id)
                .and_then(|arc| arc.faction_id)
                .map_or(agent_faction_id, i64::from);
            self.handled_active_ids.insert(m.mission_id);
            self.handled_mission_ids.insert(m.mission_id);
            let emit = EpicArcEmitCtx {
                arc_id,
                faction_id,
                chapter: 1,
                state: "Active",
                on_cooldown: false,
                chosen_branch: &PyRep::None,
                cooldown_until: &PyRep::None,
                now_unix: self.now_unix,
            };
            self.by_arc
                .entry(arc_id)
                .or_default()
                .push(active_epic_arc_mission_entry(
                    self.snap,
                    self.char_id,
                    m,
                    &emit,
                ));
        }
        self.by_arc
            .into_iter()
            .map(|(arc_id, missions)| (PyRep::Integer(i64::from(arc_id)), PyRep::Dict(missions)))
            .collect()
    }
}

fn build_epic_arc_status_dict(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    now_unix: u64,
) -> Vec<(PyRep, PyRep)> {
    let mut collector = EpicArcStatusCollector::new(ctx, snap, char_id, now_unix);
    for entry in load_epic_arc_states(snap, char_id) {
        collector.push_recorded_entry(&entry);
    }
    collector.finish()
}

fn mission_msg_dict<T>(entries: &[T], pick: fn(&T) -> (u32, i64)) -> PyRep {
    PyRep::Dict(
        entries
            .iter()
            .map(|entry| {
                let (mission_id, msg_id) = pick(entry);
                (PyRep::Integer(i64::from(mission_id)), PyRep::Integer(msg_id))
            })
            .collect(),
    )
}

fn build_epic_arc_messages_rep() -> PyRep {
    PyRep::Dict(vec![
        (
            PyRep::String("messages.epicMission.journalText.chapterTitle".into()),
            mission_msg_dict(EPIC_ARC_CHAPTER_TITLES, |&(mid, msg_id)| (mid, msg_id)),
        ),
        (
            PyRep::String("messages.epicMission.journalText.inProgressMessage".into()),
            mission_msg_dict(EPIC_ARC_MISSION_SPECS, |&(mid, _, _, msg_id, _)| {
                (mid, msg_id)
            }),
        ),
        (
            PyRep::String("messages.epicMission.journalText.completedMessage".into()),
            mission_msg_dict(EPIC_ARC_MISSION_SPECS, |&(mid, _, _, _, msg_id)| {
                (mid, msg_id)
            }),
        ),
    ])
}

pub fn is_pirate_entity(ctx: &ServerContext, entity_id: i64) -> bool {
    let is_pirate_fac = |fid: i64| matches!(fid, 500_010 | 500_011 | 500_012 | 500_019 | 500_020);
    if is_pirate_fac(entity_id) {
        return true;
    }
    if let Some(fid) = ctx.sde.npc_corporations.get(&entity_id).and_then(|c| c.faction_id) {
        return is_pirate_fac(fid as i64);
    }
    if let Some(agent) = ctx.sde.agents.get(&entity_id) {
        if let Some(fid) = ctx.sde.npc_corporations.get(&agent.corporation_id).and_then(|c| c.faction_id) {
            return is_pirate_fac(fid as i64);
        }
    }
    false
}

pub fn is_trig_or_edencom_entity(ctx: &ServerContext, entity_id: i64) -> bool {
    let is_te_fac = |fid: i64| matches!(fid, 500_026 | 500_027);
    if is_te_fac(entity_id) {
        return true;
    }
    if let Some(fid) = ctx.sde.npc_corporations.get(&entity_id).and_then(|c| c.faction_id) {
        return is_te_fac(i64::from(fid));
    }
    if let Some(agent) = ctx.sde.agents.get(&entity_id) {
        if let Some(fid) = ctx.sde.npc_corporations.get(&agent.corporation_id).and_then(|c| c.faction_id) {
            return is_te_fac(i64::from(fid));
        }
    }
    false
}

pub fn compute_effective_standing_breakdown(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    entity_id: i64,
    raw_standing: f64,
    has_entry: bool,
) -> (f64, f64, f64, f64) {
    if is_trig_or_edencom_entity(ctx, entity_id) {
        return (raw_standing.clamp(-10.0, 10.0), 0.0, 0.0, 0.0);
    }
    let sk = snap.skills.get(&char_id);
    let conn_lvl = sk
        .and_then(|m| m.get(&SKILL_CONNECTIONS))
        .map(|s| s.level.min(5))
        .unwrap_or(0);
    let dip_lvl = sk
        .and_then(|m| m.get(&SKILL_DIPLOMACY))
        .map(|s| s.level.min(5))
        .unwrap_or(0);
    let crim_lvl = sk
        .and_then(|m| m.get(&SKILL_CRIMINAL_CONNECTIONS))
        .map(|s| s.level.min(5))
        .unwrap_or(0);

    if raw_standing < 0.0 {
        let dip_bonus = (10.0 - raw_standing) * 0.04 * (dip_lvl as f64);
        let eff = (raw_standing + dip_bonus).clamp(-10.0, 10.0);
        (eff, 0.0, dip_bonus, 0.0)
    } else if has_entry || raw_standing > 0.0 {
        if is_pirate_entity(ctx, entity_id) {
            let crim_bonus = (10.0 - raw_standing) * 0.04 * (crim_lvl as f64);
            let eff = (raw_standing + crim_bonus).clamp(-10.0, 10.0);
            (eff, 0.0, 0.0, crim_bonus)
        } else {
            let conn_bonus = (10.0 - raw_standing) * 0.04 * (conn_lvl as f64);
            let eff = (raw_standing + conn_bonus).clamp(-10.0, 10.0);
            (eff, conn_bonus, 0.0, 0.0)
        }
    } else {
        (0.0, 0.0, 0.0, 0.0)
    }
}

pub fn compute_effective_agent_standing(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    agent_id: i64,
    corp_id: i64,
    faction_id: Option<i64>,
) -> f64 {
    let Some(ch) = snap.characters.get(&char_id) else {
        return 0.0;
    };
    let mut vals = Vec::with_capacity(3);
    if let Some(&raw) = ch.standings.get(&agent_id) {
        let (eff, _, _, _) =
            compute_effective_standing_breakdown(ctx, snap, char_id, agent_id, raw, true);
        vals.push(eff);
    }
    if let Some(&raw) = ch.standings.get(&corp_id) {
        let (eff, _, _, _) =
            compute_effective_standing_breakdown(ctx, snap, char_id, corp_id, raw, true);
        vals.push(eff);
    }
    if let Some(fid) = faction_id {
        if let Some(&raw) = ch.standings.get(&fid) {
            let (eff, _, _, _) =
                compute_effective_standing_breakdown(ctx, snap, char_id, fid, raw, true);
            vals.push(eff);
        }
    }
    if vals.is_empty() {
        return 0.0;
    }
    if vals.iter().any(|&v| v <= -2.0) {
        return vals.into_iter().fold(10.0_f64, f64::min);
    }
    vals.into_iter().fold(-10.0_f64, f64::max)
}

// Authoritative inter-faction standing relations on EVE's `[-10.0, +10.0]` scale (`from_id`, `to_id`, `standing`).
// Provenance: Crucible 1.6 SQL dump `repStandings.sql.gz` (`fromID`, `toID`, `standing` for NPC factions
// `500001..=500020`, 292 non-zero directed pairs) plus the 2 V24 Triglavian/EDENCOM directed hostility pairs
// `(500_026, 500_027, -10.0)` and `(500_027, 500_026, -10.0)` (294 total tuples).
pub const SYNTHESIZED_NPC_FACTION_STANDINGS: &[(i64, i64, f64)] = &[
    (500_001, 500_002, -2.0),
    (500_001, 500_003, 7.0),
    (500_001, 500_004, -5.0),
    (500_001, 500_005, 1.8),
    (500_001, 500_006, 3.0),
    (500_001, 500_007, 4.0),
    (500_001, 500_008, 4.5),
    (500_001, 500_009, -2.0),
    (500_001, 500_010, -9.0),
    (500_001, 500_012, -7.0),
    (500_001, 500_013, 1.5),
    (500_001, 500_014, -2.0),
    (500_001, 500_015, -2.0),
    (500_001, 500_016, -0.2),
    (500_001, 500_017, 1.0),
    (500_001, 500_018, 9.0),
    (500_001, 500_019, -7.0),
    (500_002, 500_001, -2.0),
    (500_002, 500_003, -5.0),
    (500_002, 500_004, 8.0),
    (500_002, 500_005, 2.5),
    (500_002, 500_006, 3.0),
    (500_002, 500_007, -3.0),
    (500_002, 500_008, -5.0),
    (500_002, 500_009, -1.0),
    (500_002, 500_011, -9.0),
    (500_002, 500_013, 1.2),
    (500_002, 500_014, 4.0),
    (500_002, 500_015, 4.0),
    (500_002, 500_016, 2.5),
    (500_002, 500_017, 2.0),
    (500_002, 500_018, -4.0),
    (500_002, 500_020, -7.0),
    (500_003, 500_001, 5.0),
    (500_003, 500_002, -5.0),
    (500_003, 500_004, -2.0),
    (500_003, 500_005, -0.5),
    (500_003, 500_006, 3.0),
    (500_003, 500_007, 9.0),
    (500_003, 500_008, 0.5),
    (500_003, 500_009, -2.0),
    (500_003, 500_010, -7.0),
    (500_003, 500_012, -8.0),
    (500_003, 500_013, 0.2),
    (500_003, 500_014, -1.2),
    (500_003, 500_015, -5.0),
    (500_003, 500_016, -1.0),
    (500_003, 500_017, -0.2),
    (500_003, 500_018, 5.0),
    (500_003, 500_019, -8.0),
    (500_004, 500_001, -5.0),
    (500_004, 500_002, 8.0),
    (500_004, 500_003, -2.0),
    (500_004, 500_005, -0.2),
    (500_004, 500_006, 3.0),
    (500_004, 500_007, -2.0),
    (500_004, 500_008, -1.0),
    (500_004, 500_009, -2.5),
    (500_004, 500_011, -8.0),
    (500_004, 500_013, 3.0),
    (500_004, 500_014, 2.0),
    (500_004, 500_015, -1.0),
    (500_004, 500_016, 8.0),
    (500_004, 500_017, 1.5),
    (500_004, 500_018, -2.0),
    (500_004, 500_020, -9.0),
    (500_005, 500_001, 1.8),
    (500_005, 500_002, 2.5),
    (500_005, 500_003, -0.5),
    (500_005, 500_004, -0.2),
    (500_005, 500_007, -0.5),
    (500_005, 500_008, -1.0),
    (500_005, 500_009, -1.0),
    (500_005, 500_013, 1.2),
    (500_005, 500_014, 1.0),
    (500_005, 500_015, 0.2),
    (500_005, 500_016, 2.0),
    (500_005, 500_017, 9.0),
    (500_005, 500_018, 0.8),
    (500_006, 500_010, -2.0),
    (500_006, 500_011, -1.0),
    (500_006, 500_012, -2.0),
    (500_006, 500_019, -3.0),
    (500_006, 500_020, -1.0),
    (500_007, 500_001, 6.0),
    (500_007, 500_002, -6.0),
    (500_007, 500_003, 9.0),
    (500_007, 500_004, -3.0),
    (500_007, 500_005, -0.5),
    (500_007, 500_006, 3.0),
    (500_007, 500_008, 0.5),
    (500_007, 500_009, -3.0),
    (500_007, 500_010, -6.0),
    (500_007, 500_011, -2.0),
    (500_007, 500_012, -9.0),
    (500_007, 500_013, 0.8),
    (500_007, 500_014, -1.0),
    (500_007, 500_015, -7.0),
    (500_007, 500_016, -0.5),
    (500_007, 500_017, -0.2),
    (500_007, 500_018, 4.0),
    (500_007, 500_019, -7.0),
    (500_008, 500_001, 6.0),
    (500_008, 500_002, -4.0),
    (500_008, 500_003, 6.0),
    (500_008, 500_004, -2.0),
    (500_008, 500_005, -1.0),
    (500_008, 500_006, 3.0),
    (500_008, 500_007, 0.5),
    (500_008, 500_009, -1.0),
    (500_008, 500_010, -3.0),
    (500_008, 500_012, -7.0),
    (500_008, 500_013, 0.8),
    (500_008, 500_014, -2.0),
    (500_008, 500_015, -3.0),
    (500_008, 500_016, -0.2),
    (500_008, 500_017, 0.2),
    (500_008, 500_018, 6.0),
    (500_008, 500_019, -6.0),
    (500_009, 500_002, -5.0),
    (500_009, 500_004, -6.0),
    (500_009, 500_005, -1.0),
    (500_009, 500_007, -3.0),
    (500_009, 500_008, -1.0),
    (500_009, 500_010, -2.0),
    (500_009, 500_011, 5.0),
    (500_009, 500_012, -1.0),
    (500_009, 500_014, 5.0),
    (500_009, 500_016, -3.0),
    (500_009, 500_017, -3.0),
    (500_009, 500_018, -6.0),
    (500_009, 500_019, -1.0),
    (500_009, 500_020, 7.0),
    (500_010, 500_003, -7.0),
    (500_010, 500_006, -2.0),
    (500_010, 500_007, -0.8),
    (500_010, 500_008, -2.0),
    (500_010, 500_009, -1.0),
    (500_010, 500_011, -3.0),
    (500_010, 500_012, 4.0),
    (500_010, 500_015, -2.0),
    (500_010, 500_017, -1.0),
    (500_010, 500_018, -8.0),
    (500_010, 500_019, 7.0),
    (500_010, 500_020, -3.0),
    (500_011, 500_002, -8.0),
    (500_011, 500_004, -7.0),
    (500_011, 500_006, -1.0),
    (500_011, 500_007, -2.5),
    (500_011, 500_008, -0.2),
    (500_011, 500_009, 4.0),
    (500_011, 500_010, -2.0),
    (500_011, 500_012, -4.0),
    (500_011, 500_014, -9.0),
    (500_011, 500_016, -2.0),
    (500_011, 500_017, -0.8),
    (500_011, 500_019, -2.0),
    (500_011, 500_020, 8.0),
    (500_012, 500_001, -7.0),
    (500_012, 500_003, -8.0),
    (500_012, 500_006, -2.0),
    (500_012, 500_007, -4.5),
    (500_012, 500_008, -3.0),
    (500_012, 500_009, -1.0),
    (500_012, 500_010, 5.0),
    (500_012, 500_011, -2.0),
    (500_012, 500_015, -2.0),
    (500_012, 500_017, -1.5),
    (500_012, 500_018, -6.0),
    (500_012, 500_019, 4.5),
    (500_012, 500_020, -2.0),
    (500_013, 500_001, 1.5),
    (500_013, 500_002, 1.2),
    (500_013, 500_003, 0.2),
    (500_013, 500_004, 3.0),
    (500_013, 500_005, 1.2),
    (500_013, 500_006, 9.0),
    (500_013, 500_007, 0.8),
    (500_013, 500_008, 0.8),
    (500_013, 500_009, -1.0),
    (500_013, 500_010, -2.0),
    (500_013, 500_011, -3.5),
    (500_013, 500_012, -3.0),
    (500_013, 500_014, 0.5),
    (500_013, 500_015, -0.2),
    (500_013, 500_016, 5.0),
    (500_013, 500_017, 1.5),
    (500_013, 500_018, 1.5),
    (500_013, 500_019, -1.8),
    (500_013, 500_020, -1.0),
    (500_014, 500_001, -2.0),
    (500_014, 500_002, 4.0),
    (500_014, 500_003, -3.0),
    (500_014, 500_004, 5.0),
    (500_014, 500_005, 1.0),
    (500_014, 500_006, 3.0),
    (500_014, 500_007, -1.0),
    (500_014, 500_008, -3.8),
    (500_014, 500_009, -1.0),
    (500_014, 500_011, -3.0),
    (500_014, 500_013, 0.5),
    (500_014, 500_015, -1.0),
    (500_014, 500_016, 0.2),
    (500_014, 500_017, -1.5),
    (500_014, 500_020, -6.0),
    (500_015, 500_001, -3.0),
    (500_015, 500_002, 3.0),
    (500_015, 500_003, -4.0),
    (500_015, 500_004, -2.0),
    (500_015, 500_005, 0.2),
    (500_015, 500_007, -7.0),
    (500_015, 500_008, -2.5),
    (500_015, 500_010, -2.0),
    (500_015, 500_012, -4.0),
    (500_015, 500_014, -1.0),
    (500_015, 500_016, -1.0),
    (500_015, 500_017, -0.5),
    (500_015, 500_018, -1.0),
    (500_015, 500_019, -3.0),
    (500_016, 500_002, 4.0),
    (500_016, 500_004, 8.0),
    (500_016, 500_005, 2.0),
    (500_016, 500_006, 5.0),
    (500_016, 500_009, -3.0),
    (500_016, 500_010, -1.0),
    (500_016, 500_011, -7.0),
    (500_016, 500_012, -3.0),
    (500_016, 500_013, 5.0),
    (500_016, 500_014, 3.0),
    (500_016, 500_015, -0.2),
    (500_016, 500_017, 6.0),
    (500_016, 500_019, -4.0),
    (500_016, 500_020, -7.0),
    (500_017, 500_001, 1.0),
    (500_017, 500_002, 2.0),
    (500_017, 500_003, -0.2),
    (500_017, 500_004, 1.5),
    (500_017, 500_005, 9.0),
    (500_017, 500_007, -0.2),
    (500_017, 500_008, 0.2),
    (500_017, 500_009, -3.0),
    (500_017, 500_010, -1.0),
    (500_017, 500_011, -0.8),
    (500_017, 500_012, -1.5),
    (500_017, 500_013, 1.5),
    (500_017, 500_014, -1.5),
    (500_017, 500_015, -0.5),
    (500_017, 500_016, 6.0),
    (500_017, 500_018, 1.5),
    (500_017, 500_019, -1.8),
    (500_017, 500_020, -1.2),
    (500_018, 500_001, 9.0),
    (500_018, 500_002, -2.0),
    (500_018, 500_003, 5.0),
    (500_018, 500_004, -3.0),
    (500_018, 500_005, 0.8),
    (500_018, 500_006, 3.0),
    (500_018, 500_007, 0.2),
    (500_018, 500_008, 1.0),
    (500_018, 500_009, -6.0),
    (500_018, 500_010, -7.0),
    (500_018, 500_011, -0.8),
    (500_018, 500_013, 1.5),
    (500_018, 500_015, -1.0),
    (500_018, 500_017, 1.5),
    (500_018, 500_019, -4.0),
    (500_019, 500_001, -7.0),
    (500_019, 500_003, -7.0),
    (500_019, 500_006, -3.0),
    (500_019, 500_007, -5.0),
    (500_019, 500_008, -5.0),
    (500_019, 500_009, -1.0),
    (500_019, 500_010, 3.0),
    (500_019, 500_011, -2.0),
    (500_019, 500_012, 5.0),
    (500_019, 500_015, -3.0),
    (500_019, 500_017, -1.8),
    (500_019, 500_018, -5.0),
    (500_019, 500_020, -2.0),
    (500_020, 500_002, -6.0),
    (500_020, 500_004, -9.0),
    (500_020, 500_006, -1.0),
    (500_020, 500_007, -1.0),
    (500_020, 500_008, -2.0),
    (500_020, 500_009, 5.0),
    (500_020, 500_010, -1.0),
    (500_020, 500_011, 8.0),
    (500_020, 500_012, -4.0),
    (500_020, 500_014, -7.0),
    (500_020, 500_016, -3.0),
    (500_020, 500_017, -1.2),
    (500_020, 500_019, -2.0),
    (500_026, 500_027, -10.0),
    (500_027, 500_026, -10.0),
];
pub fn static_npc_faction_standings() -> &'static [(i64, i64, f64)] { SYNTHESIZED_NPC_FACTION_STANDINGS }
pub fn get_sde_npc_npc_standings(sde: &eve_sde::SdeRegistry) -> Vec<(i64, i64, f64)> {
    let mut rows: Vec<(i64, i64, f64)> = SYNTHESIZED_NPC_FACTION_STANDINGS
        .iter()
        .copied()
        .filter(|&(from_f, to_f, _)| {
            sde.factions.is_empty()
                || (sde.factions.contains_key(&(from_f as u32))
                    && sde.factions.contains_key(&(to_f as u32)))
        })
        .collect();
    if !sde.npc_corporations.is_empty() {
        let mut corp_ids: Vec<i64> = sde.npc_corporations.keys().copied().collect();
        corp_ids.sort_unstable();
        for cid in corp_ids {
            if let Some(fid) = sde.npc_corporations.get(&cid).and_then(|c| c.faction_id) {
                let fid_i64 = i64::from(fid);
                rows.push((cid, fid_i64, 10.0));
                rows.push((fid_i64, cid, 10.0));
            }
        }
    }
    rows
}

/// Returns true iff `id` is a valid EVE owner ID (`idCheckers.IsOwner`: faction, NPC corp,
/// agent/NPC character, player character, player corp, alliance, or an owner registered in
/// SDE/world state). Prevents non-owner IDs (`<= 0`, solar systems, stations, item IDs, junk)
/// from entering `standingMgr` rowsets where `cfg.eveowners.Prime(...)` would crash.
pub(crate) fn is_valid_standing_owner_id(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    id: i64,
) -> bool {
    if id <= 0 {
        return false;
    }
    (500_000..=599_999).contains(&id)
        || (1_000_000..=1_999_999).contains(&id)
        || (3_000_000..=3_999_999).contains(&id)
        || (90_000_000..=99_999_999).contains(&id)
        || (2_100_000_000..=2_129_999_999).contains(&id)
        || snap.characters.contains_key(&id)
        || snap.corporations.contains_key(&id)
        || snap.alliances.contains_key(&id)
        || u32::try_from(id).ok().is_some_and(|fid| ctx.sde.factions.contains_key(&fid))
        || ctx.sde.npc_corporations.contains_key(&id)
        || ctx.sde.agents.contains_key(&id)
}

/// Applies a primary faction standing percentage change (`fac_pct`, positive for gain or
/// negative for loss) and, when `apply_derived` is true, propagates derived faction standings
/// per EVE Wiki (`NPC_standings.md`):
/// - `Inter-Faction Standing Modifier = Secondary's view of Primary * (Primary Size / Secondary Size)`
/// - `Derived Standing % = Gain % * (Inter-Faction Standing Modifier / 10.0)`
/// - Gains and enemy losses are capped at Secondary's standing view of Primary (`rel`).
/// - When `fac_pct < 0.0` (faction standing loss), friends (`rel > 0.0`) receive negative derived
///   standings while enemies (`rel < 0.0`) do not receive positive derived standings.
pub fn apply_faction_standing_with_derived(
    ctx: &ServerContext,
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    primary_faction_id: i64,
    fac_pct: f64,
    apply_derived: bool,
    changes: &mut Vec<(i64, f64, f64)>,
) {
    if fac_pct.abs() <= 1e-9 {
        return;
    }
    let Some(ch) = snap.characters.get_mut(&char_id) else {
        return;
    };
    let cur_fac = ch.standings.get(&primary_faction_id).copied().unwrap_or(0.0);
    let new_fac = if fac_pct >= 0.0 {
        apply_standing_increase(cur_fac, fac_pct)
    } else {
        apply_standing_decrease(cur_fac, fac_pct.abs())
    };
    let fac_delta = new_fac - cur_fac;
    ch.standings.insert(primary_faction_id, new_fac);
    if fac_delta.abs() > 1e-9 {
        changes.push((primary_faction_id, new_fac, fac_delta));
    }
    if !apply_derived {
        return;
    }
    let primary_size = ctx.sde.factions.get(&(primary_faction_id as u32)).map_or(5.0, |f| f.size_factor);
    if primary_size <= 0.0 {
        return;
    }
    for &(secondary_f, target_f, rel) in SYNTHESIZED_NPC_FACTION_STANDINGS {
        if target_f != primary_faction_id || rel == 0.0 {
            continue;
        }
        let secondary_size = ctx.sde.factions.get(&(secondary_f as u32)).map_or(5.0, |f| f.size_factor);
        if secondary_size <= 0.0 {
            continue;
        }
        let modifier = rel * (primary_size / secondary_size);
        let derived_pct = fac_pct * (modifier / 10.0);
        let cur_other = ch.standings.get(&secondary_f).copied().unwrap_or(0.0);
        let new_other = if fac_pct > 0.0 {
            if rel > 0.0 {
                if cur_other >= rel { cur_other } else { apply_standing_increase(cur_other, derived_pct).min(rel) }
            } else if cur_other <= rel {
                cur_other
            } else {
                apply_standing_decrease(cur_other, derived_pct.abs()).max(rel)
            }
        } else if rel > 0.0 {
            apply_standing_decrease(cur_other, derived_pct.abs())
        } else {
            cur_other
        };
        let d_other = new_other - cur_other;
        if d_other.abs() > 1e-6 {
            ch.standings.insert(secondary_f, new_other);
            changes.push((secondary_f, new_other, d_other));
        }
    }
}

pub fn apply_mission_completion_standings(
    ctx: &ServerContext,
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    agent_id: i64,
    mission_id: u32,
    corp_id: i64,
    base_standing_pct: f64,
    is_storyline_or_special: bool,
) -> (f64, Vec<(i64, f64, f64)>) {
    let social_lvl = snap.skills.get(&char_id)
        .and_then(|m| m.get(&SKILL_SOCIAL))
        .map(|s| s.level.min(5))
        .unwrap_or(0);
    let scaled_pct = base_standing_pct * (1.0 + 0.05 * (social_lvl as f64));
    let mut changes: Vec<(i64, f64, f64)> = Vec::new();
    let mut new_corp_st = 0.0;

    if let Some(ch) = snap.characters.get_mut(&char_id) {
        let cur_corp = ch.standings.get(&corp_id).copied().unwrap_or(0.0);
        new_corp_st = apply_standing_increase(cur_corp, scaled_pct);
        ch.standings.insert(corp_id, new_corp_st);
        let corp_delta = new_corp_st - cur_corp;
        changes.push((corp_id, new_corp_st, corp_delta));

        if agent_id > 0 && scaled_pct > 0.0 {
            let cur_agent = ch.standings.get(&agent_id).copied().unwrap_or(0.0);
            let new_agent = apply_standing_increase(cur_agent, scaled_pct);
            ch.standings.insert(agent_id, new_agent);
            changes.push((agent_id, new_agent, new_agent - cur_agent));
        }
    }

    let faction_id = ctx.sde.npc_corporations.get(&corp_id).and_then(|c| c.faction_id).map(|f| f as i64);

    if scaled_pct.abs() > 1e-9 {
        if let Some(fid) = faction_id {
            let fac_pct = if is_storyline_or_special { scaled_pct * 0.50 } else { scaled_pct * 0.15 };
            // Per EVE Wiki (NPC_standings.md): Epic Arc (agent_type_id == 10) missions and only the
            // final mission of a Career Agent (agent_type_id == 12) chain suppress derived standings.
            let agent_type = ctx.sde.agents.get(&agent_id).map_or(0, |a| a.agent_type_id);
            let is_final_career = agent_type == 12
                && ctx.sde.missions.get(&mission_id).is_some_and(|m| {
                    m.title.ends_with("(10 of 10)")
                        || m.title.ends_with("(5 of 5)")
                        || m.title.eq_ignore_ascii_case("Song Of Birds")
                });
            let apply_derived = agent_type != 10 && !is_final_career;
            apply_faction_standing_with_derived(ctx, snap, char_id, fid, fac_pct, apply_derived, &mut changes);
        }
    }

    for &(from_id, _new_val, delta) in &changes {
        if delta.abs() > 1e-9 {
            snap.next_entity_id += 1;
            let ev_id = snap.next_entity_id;
            snap.standing_transactions
                .entry(char_id)
                .or_default()
                .push(StandingTransactionRecord {
                    event_id: ev_id,
                    from_id,
                    to_id: char_id,
                    modification: delta,
                    event_type_id: if delta >= 0.0 { 85 } else { 83 },
                });
        }
    }

    (new_corp_st, changes)
}

pub fn notify_standing_changes(
    ctx: &ServerContext,
    char_id: i64,
    changes: &[(i64, f64, f64)],
) {
    if changes.is_empty() {
        return;
    }
    let mods: Vec<PyRep> = changes
        .iter()
        .map(|&(from_id, _new_val, delta)| {
            PyRep::Tuple(vec![
                PyRep::Integer(from_id),
                PyRep::Integer(char_id),
                PyRep::Real(delta),
                PyRep::Real(-10.0),
                PyRep::Real(10.0),
            ])
        })
        .collect();
    if let Some(&(first_from, first_val, _)) = changes.first() {
        ctx.emit_typed(
            NotificationTarget::Character(char_id),
            OnStandingSet {
                from_id: first_from,
                to_id: char_id,
                standing: first_val,
            },
        );
    }
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnStandingsModified {
            modifications: mods,
        },
    );
}

#[allow(clippy::too_many_arguments)]
pub fn handle_special_dialogue_action(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    sys_id: u32,
    action_id: i64,
    agent_id: i64,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match action_id {
        // 12 = agentDialogueButtonStartResearch (feat:pve.research-agent-rp-formula; see GAP(sde) below for access-requirements)
        12 => {
            let Some(agent_info) = ctx.sde.agents.get(&agent_id) else {
                return Some(Err(BridgeRpcError { code: "UnknownAgent".into(), message: format!("Agent {agent_id} does not exist in SDE"), client_error: None }));
            };
            if agent_info.agent_type_id != 4 {
                return Some(Err(BridgeRpcError { code: "NotResearchAgent".into(), message: format!("Agent {agent_id} is not an R&D research agent"), client_error: None }));
            }
            let requested_field_skill: Option<TypeId> = match args.get(2) {
                None | Some(PyRep::None) => None,
                Some(arg) => match arg.as_i64().and_then(|v| TypeId::try_from(v).ok()) {
                    Some(sk) => Some(sk),
                    None => {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidResearchSkill".into(),
                            message: "Research skill argument must be a valid skill typeID".into(),
                            client_error: None,
                        }));
                    }
                },
            };
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let (req_field_skill, skill_lvl) =
                    crate::services::pve_ess_reserve_banks::validate_rnd_research_access(
                        ctx,
                        session.station_id,
                        sys_id,
                        snap,
                        char_id,
                        agent_info,
                        requested_field_skill,
                    )?;
                let rp_per_day = crate::services::pve_ess_reserve_banks::compute_rnd_rp_per_day(
                    ctx,
                    snap,
                    char_id,
                    agent_id,
                    agent_info.level,
                    skill_lvl,
                );
                let mut entries = load_rnd_research_entries(snap, char_id);
                if let Some(existing) = entries.iter_mut().find(|e| e.agent_id == agent_id) {
                    let elapsed_days = (now_unix.saturating_sub(existing.started_unix) as f64) / 86_400.0;
                    existing.initial_points += existing.points_per_day * elapsed_days;
                    existing.points_per_day = rp_per_day;
                    existing.research_type_id = req_field_skill;
                    existing.started_unix = now_unix;
                } else {
                    entries.push(RndResearchEntry {
                        agent_id,
                        research_type_id: req_field_skill,
                        points_per_day: rp_per_day,
                        initial_points: 0.0,
                        started_unix: now_unix,
                    });
                }
                save_rnd_research_entries(snap, char_id, &entries);
                Ok((req_field_skill, rp_per_day))
            });
            match res {
                Ok((req_field_skill, rp_per_day)) => Some(Ok(PyRep::keyval([
                    ("agentID", PyRep::Integer(agent_id)),
                    ("researchTypeID", PyRep::Integer(req_field_skill as i64)),
                    ("pointsPerDay", PyRep::Real(rp_per_day)),
                    ("researchStarted", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        // 13 = agentDialogueButtonCancelResearch
        13 => {
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut entries = load_rnd_research_entries(snap, char_id);
                let before = entries.len();
                entries.retain(|e| e.agent_id != agent_id);
                if entries.len() == before {
                    return Err(BridgeRpcError { code: "ResearchNotActive".into(), message: format!("No active R&D research with agent {agent_id}"), client_error: None });
                }
                save_rnd_research_entries(snap, char_id, &entries);
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("agentID", PyRep::Integer(agent_id)),
                    ("researchCancelled", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        // 14 = agentDialogueButtonBuyDatacores (feat:pve.research-agents)
        14 => Some(crate::services::notify_emitters_skills_trade_godma::handle_rnd_buy_datacores(
            ctx, session, char_id, agent_id, sys_id, args,
        )),

        // 15 | 16 | 17 = Locator Agent service (feat:pve.locator-agents)
        15..=17 => {
            if action_id == 17 {
                return Some(Ok(PyRep::keyval([
                    ("agentID", PyRep::Integer(agent_id)),
                    ("locateRejected", PyRep::Bool(true)),
                ])));
            }
            let Some(agent_info) = ctx.sde.agents.get(&agent_id) else {
                return Some(Err(BridgeRpcError { code: "UnknownAgent".into(), message: format!("Agent {agent_id} does not exist in SDE"), client_error: None }));
            };
            if !agent_info.is_locator {
                return Some(Err(BridgeRpcError { code: "AgentHasNoLocatorService".into(), message: format!("Agent {agent_id} does not offer Locator services"), client_error: None }));
            }
            let caller_sys = if sys_id > 0 {
                sys_id
            } else {
                session
                    .solar_system_id
                    .or_else(|| ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id)))
                    .unwrap_or(0)
            };
            if agent_info.solar_system_id != Some(caller_sys)
                || (session.station_id.is_some()
                    && agent_info.station_id.is_some()
                    && agent_info.station_id != session.station_id)
            {
                return Some(Err(BridgeRpcError {
                    code: "AgentNotCoLocated".into(),
                    message: format!(
                        "Must be co-located with Locator Agent {agent_id} in system {:?} / station {:?}",
                        agent_info.solar_system_id, agent_info.station_id
                    ),
                    client_error: None,
                }));
            }
            let target_char_id = args
                .get(2)
                .or_else(|| args.get(1))
                .and_then(|a| a.as_i64())
                .filter(|&id| id != agent_id)
                .unwrap_or(90_000_002);
            let now_unix = ctx.now_unix();
            let (isk_fee, cooldown_sec, precision_desc) = match agent_info.level {
                1 => (2_000_i64, 900_u64, "SameConstellation"),
                2 => (8_000, 480, "SameRegion"),
                3 => (25_000, 240, "KnownSpace"),
                4 => (75_000, 120, "AllKnownSpace"),
                _ => (150_000, 60, "AllKnownSpaceImmediate"),
            };
            let cd_nid = locator_cooldown_note_id(char_id, agent_id);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let eff_st = compute_effective_agent_standing(ctx, snap, char_id, agent_id, agent_info.corporation_id, None);
                if agent_info.level > 1 {
                    let req_st = ((agent_info.level as f64) - 1.0) * 2.0 - 1.0;
                    if eff_st < req_st {
                        return Err(BridgeRpcError { code: "InsufficientAgentStanding".into(), message: format!("Locator agent {agent_id} (level {}) requires effective standing >= {req_st:.1} (have {eff_st:.2})", agent_info.level), client_error: None });
                    }
                } else if eff_st <= -2.0 {
                    return Err(BridgeRpcError { code: "InsufficientAgentStanding".into(), message: format!("Locator agent {agent_id} (level 1) requires effective standing > -2.0 (have {eff_st:.2})"), client_error: None });
                }
                if let Some(cd_note) = snap.owner_notes.get(&cd_nid) {
                    if now_unix < cd_note.timestamp_unix {
                        return Err(BridgeRpcError { code: "LocatorAgentOnCooldown".into(), message: format!("Locator agent {agent_id} is on cooldown until {}", cd_note.timestamp_unix), client_error: None });
                    }
                }
                let Some(target_ch) = snap.characters.get(&target_char_id).cloned() else {
                    return Err(BridgeRpcError { code: "TargetCharacterNotFound".into(), message: format!("Target character {target_char_id} not found"), client_error: None });
                };
                if target_ch.solar_system_id >= 31_000_000 {
                    return Err(BridgeRpcError {
                        code: "LocatorTargetInUnknownSpace".into(),
                        message: format!("Locator agents cannot locate targets in wormhole or abyssal space (system {})", target_ch.solar_system_id),
                        client_error: None,
                    });
                }
                let sys_info = ctx.sde.solar_systems.get(&target_ch.solar_system_id);
                let const_id = sys_info.map(|s| s.constellation_id).unwrap_or(target_ch.constellation_id);
                let reg_id = sys_info.map(|s| s.region_id).unwrap_or(target_ch.region_id);
                if reg_id >= 11_000_000 {
                    return Err(BridgeRpcError {
                        code: "LocatorTargetInUnknownSpace".into(),
                        message: format!("Locator agents cannot locate targets in unknown space region {reg_id}"),
                        client_error: None,
                    });
                }
                let agent_sys_info = agent_info.solar_system_id.and_then(|sid| ctx.sde.solar_systems.get(&sid));
                let agent_const_id = agent_sys_info.map(|s| s.constellation_id).unwrap_or(target_ch.constellation_id);
                let agent_reg_id = agent_sys_info.map(|s| s.region_id).unwrap_or(target_ch.region_id);
                if agent_info.level == 1 && const_id != agent_const_id {
                    return Err(BridgeRpcError {
                        code: "LocatorTargetOutOfRange".into(),
                        message: format!("Level 1 locator agent {agent_id} can only locate targets in constellation {agent_const_id}"),
                        client_error: None,
                    });
                }
                if agent_info.level == 2 && reg_id != agent_reg_id {
                    return Err(BridgeRpcError {
                        code: "LocatorTargetOutOfRange".into(),
                        message: format!("Level 2 locator agent {agent_id} can only locate targets in region {agent_reg_id}"),
                        client_error: None,
                    });
                }
                let fee_cents = IskCents::from_isk(isk_fee);
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    if ch.balance < fee_cents {
                        return Err(BridgeRpcError { code: "InsufficientFunds".into(), message: format!("Locator service requires {isk_fee} ISK"), client_error: None });
                    }
                    ch.balance = ch.balance.checked_sub(fee_cents).unwrap_or(IskCents(0));
                    let bal_after = ch.balance;
                    snap.append_journal(
                        char_id,
                        "AgentLocationServices",
                        fee_cents,
                        bal_after,
                        format!("Locator agent {agent_id} fee for character {target_char_id}"),
                    );
                }
                snap.owner_notes.insert(
                    cd_nid,
                    eve_db::OwnerNoteRecord {
                        note_id: cd_nid,
                        character_id: -5,
                        item_id: agent_id,
                        label: format!("locator_cd:{char_id}:{agent_id}"),
                        note: target_char_id.to_string(),
                        timestamp_unix: now_unix + cooldown_sec,
                    },
                );
                Ok((
                    target_ch.solar_system_id,
                    const_id,
                    reg_id,
                    target_ch.station_id,
                ))
            });
            match res {
                Ok((t_sys, t_const, t_reg, t_sta)) => Some(Ok(PyRep::keyval([
                    ("agentID", PyRep::Integer(agent_id)),
                    ("targetCharacterID", PyRep::Integer(target_char_id)),
                    ("solarSystemID", PyRep::Integer(t_sys as i64)),
                    ("constellationID", PyRep::Integer(t_const as i64)),
                    ("regionID", PyRep::Integer(t_reg as i64)),
                    ("stationID", t_sta.map(PyRep::Integer).unwrap_or(PyRep::None)),
                    ("agentLevel", PyRep::Integer(agent_info.level as i64)),
                    ("precision", PyRep::String(precision_desc.into())),
                    ("iskFee", PyRep::Integer(isk_fee)),
                    ("cooldownSeconds", PyRep::Integer(cooldown_sec as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        // 18 | 19 = agentDialogueButtonYes / No (Epic Arc choice branching: feat:pve.epic-arcs, feat:pve.storyline-arcs-soe)
        18 | 19 => {
            let branch_name = args
                .get(2)
                .and_then(|a| a.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    if action_id == 18 {
                        "Amarr".into()
                    } else {
                        "Minmatar".into()
                    }
                });
            let (arc_id, faction_id) = resolve_epic_arc_id_for_agent(ctx, agent_id);
            ctx.db.with_mut_snapshot(|snap| {
                let mut arcs = load_epic_arc_states(snap, char_id);
                if let Some(arc) = arcs.iter_mut().find(|a| a.epic_arc_id == arc_id) {
                    arc.chosen_branch = Some(branch_name.clone());
                    arc.chapter = arc.chapter.max(2);
                } else {
                    arcs.push(EpicArcStateEntry {
                        epic_arc_id: arc_id,
                        agent_id,
                        faction_id,
                        chapter: 2,
                        completed_missions: Vec::new(),
                        chosen_branch: Some(branch_name.clone()),
                        completed_at_unix: None,
                        cooldown_until_unix: None,
                    });
                }
                save_epic_arc_states(snap, char_id, &arcs);
            });
            Some(Ok(PyRep::keyval([
                ("epicArcID", PyRep::Integer(arc_id as i64)),
                ("agentID", PyRep::Integer(agent_id)),
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("chosenBranch", PyRep::String(branch_name)),
                ("repeatCooldownSeconds", PyRep::Integer(7_776_000)),
            ])))
        }

        _ => None,
    }
}

pub async fn handle_extended(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_extended_with_bound(ctx, session, None, service, method, args, _kwargs).await
}

pub(super) async fn handle_extended_with_bound(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&crate::router::BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("agentMgr", "agentID") => handle_agent_mgr_attr(ctx, session, bound_entry, service, "agentID", args),
        ("agentMgr", "corporationID") => handle_agent_mgr_attr(ctx, session, bound_entry, service, "corporationID", args),
        ("agentMgr", "factionID") => handle_agent_mgr_attr(ctx, session, bound_entry, service, "factionID", args),
        ("agentMgr", "solarsystemID") => handle_agent_mgr_attr(ctx, session, bound_entry, service, "solarsystemID", args),
        ("agentMgr", "stationID") => handle_agent_mgr_attr(ctx, session, bound_entry, service, "stationID", args),

        ("agentMgr", "GetCompletedCareerAgentIDs") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut requested_ids = Vec::new();
            let mut seen_ids = std::collections::HashSet::new();
            if let Some(arg0) = args.first().or_else(|| kwargs.get("agentIDs")) {
                match arg0 {
                    PyRep::None => {}
                    PyRep::List(items)
                    | PyRep::Tuple(items)
                    | PyRep::Set(items)
                    | PyRep::PySet(items) => {
                        for item in items {
                            let Some(aid) = item.as_i64().filter(|&id| id > 0) else {
                                return Some(Err(BridgeRpcError {
                                    code: "InvalidParameter".into(),
                                    message: "agentIDs must contain positive integer agent IDs".into(),
                                    client_error: None,
                                }));
                            };
                            if seen_ids.insert(aid) {
                                requested_ids.push(aid);
                            }
                        }
                    }
                    PyRep::Integer(aid) if *aid > 0 => {
                        requested_ids.push(*aid);
                    }
                    _ => {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidParameter".into(),
                            message: "agentIDs must be a list, tuple, set, or positive integer".into(),
                            client_error: None,
                        }));
                    }
                }
            }
            // V24.01 `agents.pyj:217-223` calls `self._careerAgentsCompletedByID.update(...)`
            // and indexes `self._careerAgentsCompletedByID[agentID]` directly, so every queried
            // `agentID` must be mapped to `PyRep::Bool(completed)` in the returned `PyRep::Dict`.
            let completed_set: std::collections::BTreeSet<i64> = ctx.db.with_snapshot(|snap| {
                snap.active_missions
                    .values()
                    .filter(|m| m.character_id == char_id && m.state == "Completed")
                    .filter_map(|m| {
                        ctx.sde
                            .agents
                            .get(&m.agent_id)
                            .filter(|a| a.agent_type_id == 12)
                            .map(|a| a.agent_id)
                    })
                    .collect()
            });
            let entries: Vec<(PyRep, PyRep)> = if requested_ids.is_empty() {
                completed_set
                    .into_iter()
                    .map(|aid| (PyRep::Integer(aid), PyRep::Bool(true)))
                    .collect()
            } else {
                requested_ids
                    .into_iter()
                    .map(|aid| (PyRep::Integer(aid), PyRep::Bool(completed_set.contains(&aid))))
                    .collect()
            };
            Some(Ok(PyRep::Dict(entries)))
        }

        // parity: rpc:agentMgr.GetDisabledMissions
        ("agentMgr", "GetDisabledMissions") => handle_get_disabled_missions(ctx, session),

        ("agentMgr", "GetReplayTimestamp") => {
            handle_get_replay_timestamp(ctx, session, bound_entry, args)
        }

        ("agentMgr", "GetPlausibleMissionIDs") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let bound_aid = bound_entry
                .filter(|b| b.service == "agentMgr" && b.object_id > 0)
                .map(|b| b.object_id);
            let explicit_aid = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0);
            let target_aid = bound_aid
                .or(explicit_aid)
                .or_else(|| {
                    ctx.bound_objects.read().ok().and_then(|map| {
                        map.values()
                            .filter(|b| b.session_id == session.session_id && b.service == "agentMgr" && b.object_id > 0)
                            .max_by_key(|b| b.bound_id)
                            .map(|b| b.object_id)
                    })
                });
            let mut agent_ids: Vec<i64> = Vec::new();
            if let Some(aid) = target_aid {
                if !ctx.sde.agents.contains_key(&aid) {
                    return Some(Err(BridgeRpcError {
                        code: "AgentNotFound".into(),
                        message: format!("Agent {aid} not found in SDE"),
                        client_error: None,
                    }));
                }
                agent_ids.push(aid);
            } else {
                ctx.db.with_snapshot(|snap| {
                    for ((cid, _), m) in &snap.active_missions {
                        if *cid == char_id && m.agent_id > 0 {
                            agent_ids.push(m.agent_id);
                        }
                    }
                });
                agent_ids.sort_unstable();
                agent_ids.dedup();
            }
            let mut plausible: Vec<u32> = Vec::new();
            for aid in agent_ids {
                if let Some(agent) = ctx.sde.agents.get(&aid) {
                    let is_heraldry = agent.agent_type_id == 13 || agent.division_id == 37;
                    let mut matching: Vec<u32> = ctx
                        .sde
                        .missions
                        .values()
                        .filter(|m| {
                            if is_heraldry {
                                m.content_template.as_deref().unwrap_or_default().contains("Heraldry")
                            } else {
                                crate::services::agents_missions::agents_missions_helpers::mission_matches_agent_pool(m, agent)
                            }
                        })
                        .map(|m| m.mission_id)
                        .collect();
                    matching.sort_unstable();
                    matching.truncate(25);
                    plausible.extend(matching);
                }
            }
            plausible.sort_unstable();
            plausible.dedup();
            plausible.truncate(25);
            Some(Ok(PyRep::List(plausible.into_iter().map(|m| PyRep::Integer(m as i64)).collect())))
        }

        ("agentMgr", "GetMyEpicArcStatus") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            let dict_entries =
                ctx.db
                    .with_snapshot(|snap| build_epic_arc_status_dict(ctx, snap, char_id, now_unix));
            Some(Ok(PyRep::Dict(dict_entries)))
        }

        ("agentMgr", "GetMessagesForEpicArcMissions") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            Some(Ok(build_epic_arc_messages_rep()))
        }

        ("epicArcStatus", "AgentHasEpicMissionsForCharacter") => {
            let (_, session_char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(agent_id) = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "epicArcStatus.AgentHasEpicMissionsForCharacter requires a positive agentID".into(),
                    client_error: None,
                }));
            };
            let char_id = session_char_id;
            let Some(agent) = ctx.sde.agents.get(&agent_id).filter(|a| a.agent_type_id == 10) else {
                return Some(Ok(PyRep::Bool(false)));
            };
            let has_active = ctx.db.with_snapshot(|snap| {
                snap.active_missions.values().any(|m| {
                    m.character_id == char_id
                        && m.agent_id == agent_id
                        && matches!(m.state.as_str(), "Offered" | "Accepted")
                })
            });
            if has_active {
                return Some(Ok(PyRep::Bool(true)));
            }
            let (arc_id, faction_id) = resolve_epic_arc_id_for_agent(ctx, agent_id);
            let now_unix = ctx.now_unix();
            let available = ctx.db.with_snapshot(|snap| {
                let eff = compute_effective_agent_standing(ctx, snap, char_id, agent_id, agent.corporation_id, Some(faction_id));
                eff > -2.0
                    && !load_epic_arc_states(snap, char_id)
                        .iter()
                        .any(|a| a.epic_arc_id == arc_id && a.cooldown_until_unix.is_some_and(|until| now_unix < until))
            });
            Some(Ok(PyRep::Bool(available)))
        }

        ("missionTrackerMgr", "GetAllMissionObjectives")
        | ("missionTrackerMgr", "UpdateAllMissions")
        | ("missionTrackerMgr", "GetMissionInfoItems") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let filter_mid = args.first().and_then(|a| a.as_i64()).and_then(|v| u32::try_from(v).ok());
            let items = ctx.db.with_snapshot(|snap| {
                let mut list: Vec<PyRep> = snap.active_missions.values()
                    .filter(|m| {
                        m.character_id == char_id
                            && matches!(m.state.as_str(), "Offered" | "Accepted")
                            && filter_mid.is_none_or(|f| f == m.mission_id)
                    })
                    .map(|m| {
                        let meta = load_mission_meta(snap, char_id, m.mission_id);
                        let obj_completed = m.npcs_remaining == 0
                            && meta.as_ref().and_then(|mt| mt.required_delivery_type_id).is_none_or(|req_tid| {
                                let req_q = meta.as_ref().map_or(1, |mt| mt.required_delivery_qty);
                                snap.items
                                    .values()
                                    .filter(|it| it.owner_id == char_id && it.type_id == req_tid)
                                    .map(|it| it.quantity)
                                    .sum::<i32>()
                                    >= req_q
                            });
                        PyRep::keyval([
                            ("missionID", PyRep::Integer(m.mission_id as i64)),
                            ("agentID", PyRep::Integer(m.agent_id)),
                            ("solarSystemID", PyRep::Integer(m.solar_system_id as i64)),
                            ("state", PyRep::String(m.state.clone())),
                            ("currentRoomID", PyRep::Integer(m.current_room_id as i64)),
                            ("npcsRemaining", PyRep::Integer(m.npcs_remaining as i64)),
                            ("objectivesCompleted", PyRep::Bool(obj_completed)),
                            (
                                "info",
                                PyRep::String(format!(
                                    "Mission {} for agent {}",
                                    m.mission_id, m.agent_id
                                )),
                            ),
                        ])
                    })
                    .collect();
                list.sort_by_key(|r| r.dict_get("missionID").and_then(|v| v.as_i64()).unwrap_or(0));
                list
            });
            if method == "UpdateAllMissions" {
                ctx.emit_typed(
                    NotificationTarget::Character(char_id),
                    OnMissionsUpdated {
                        missions: items.clone(),
                    },
                );
            }
            Some(Ok(PyRep::List(items)))
        }

        ("missionTrackerMgr", "IsInActiveDungeonID") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let sys_id = session.solar_system_id.unwrap_or(0);
            let in_dungeon = ctx.db.with_snapshot(|snap| {
                snap.active_missions.values().any(|m| {
                    m.character_id == char_id && m.state == "Accepted" && m.solar_system_id == sys_id
                })
            });
            Some(Ok(PyRep::Bool(in_dungeon)))
        }
        ("standingMgr", "GetCorpStandings") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let rows = ctx.db.with_snapshot(|snap| {
                let corp_id = snap.characters.get(&char_id).map_or(1_000_009, |c| c.corporation_id);
                let mut agg: HashMap<i64, (f64, usize)> = HashMap::new();
                for ch in snap.characters.values().filter(|c| c.corporation_id == corp_id) {
                    for (&from_id, &val) in &ch.standings {
                        if !is_valid_standing_owner_id(ctx, snap, from_id) {
                            continue;
                        }
                        let e = agg.entry(from_id).or_insert((0.0, 0));
                        e.0 += val;
                        e.1 += 1;
                    }
                }
                let mut entries: Vec<(i64, f64)> = agg
                    .into_iter()
                    .map(|(fid, (sum, cnt))| (fid, sum / (cnt.max(1) as f64)))
                    .collect();
                entries.sort_by_key(|&(fid, _)| fid);
                entries.into_iter().map(|(from_id, st)| vec![PyRep::Integer(from_id), PyRep::Real(st)]).collect::<Vec<_>>()
            });
            Some(Ok(PyRep::rowset(vec!["fromID", "standing"], rows)))
        }

        ("standingMgr", "GetNPCNPCStandings") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                get_sde_npc_npc_standings(&ctx.sde)
                    .into_iter()
                    .filter(|&(from_id, to_id, _)| {
                        is_valid_standing_owner_id(ctx, snap, from_id)
                            && is_valid_standing_owner_id(ctx, snap, to_id)
                    })
                    .map(|(from_id, to_id, st)| vec![PyRep::Integer(from_id), PyRep::Integer(to_id), PyRep::Real(st)])
                    .collect()
            });
            Some(Ok(PyRep::rowset(vec!["fromID", "toID", "standing"], rows)))
        }

        ("standingMgr", "GetStandingCompositions") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let target_from = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0);
            let target_to = args
                .get(1)
                .and_then(|a| a.as_i64())
                .filter(|&id| id > 0)
                .unwrap_or(char_id);
            let res = ctx.db.with_snapshot(|snap| {
                if !is_valid_standing_owner_id(ctx, snap, target_to) {
                    return PyRep::List(vec![]);
                }
                let Some(ch) = snap.characters.get(&target_to).or_else(|| snap.characters.get(&char_id)) else {
                    return PyRep::List(vec![]);
                };
                let mut entities: Vec<i64> = if let Some(fid) = target_from {
                    if is_valid_standing_owner_id(ctx, snap, fid) {
                        vec![fid]
                    } else {
                        vec![]
                    }
                } else {
                    ch.standings
                        .keys()
                        .copied()
                        .filter(|&fid| is_valid_standing_owner_id(ctx, snap, fid))
                        .collect()
                };
                entities.sort_unstable();
                let list: Vec<PyRep> = entities
                    .into_iter()
                    .map(|from_id| {
                        let raw = ch.standings.get(&from_id).copied().unwrap_or(0.0);
                        let has_entry = ch.standings.contains_key(&from_id);
                        let (eff, conn_b, dip_b, crim_b) = compute_effective_standing_breakdown(
                            ctx, snap, ch.character_id, from_id, raw, has_entry,
                        );
                        PyRep::keyval([
                            ("fromID", PyRep::Integer(from_id)),
                            ("toID", PyRep::Integer(target_to)),
                            ("rawStanding", PyRep::Real(raw)),
                            ("standing", PyRep::Real(raw)),
                            ("effectiveStanding", PyRep::Real(eff)),
                            ("connectionsBonus", PyRep::Real(conn_b)),
                            ("diplomacyBonus", PyRep::Real(dip_b)),
                            ("criminalConnectionsBonus", PyRep::Real(crim_b)),
                        ])
                    })
                    .collect();
                PyRep::List(list)
            });
            Some(Ok(res))
        }

        ("keeper", "ActivateAccelerationGate") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(mission_id) = args.first().and_then(|a| a.as_i64()).and_then(|raw_id| u32::try_from(raw_id).ok()).filter(|&mid| {
                ctx.db.with_snapshot(|snap| snap.active_missions.contains_key(&(char_id, mid)))
            }) else {
                return None;
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError { code: "ShipDocked".into(), message: "Cannot activate dungeon acceleration gate while docked".into(), client_error: None }));
            }
            if let Some(ship_id) = session.ship_id {
                let is_burner_gate = ctx.db.with_snapshot(|snap| {
                    load_mission_meta(snap, char_id, mission_id).is_some_and(|mt| mt.is_anomic_burner)
                        || ctx.sde.missions.get(&mission_id).is_some_and(|t| t.title.contains("Anomic"))
                });
                let kind = if is_burner_gate {
                    crate::services::space_nav_movement_guard::AccelerationGateKind::AnomicBurner
                } else {
                    crate::services::space_nav_movement_guard::AccelerationGateKind::DedOrExploration
                };
                if let Err(e) = ctx
                    .validate_ship_movement(
                        char_id,
                        ship_id,
                        crate::services::space_nav_movement_guard::MovementIntent::AccelerationGate {
                            sys_id: session.solar_system_id.unwrap_or(30_000_142),
                            kind,
                        },
                    )
                    .await
                {
                    return Some(Err(e));
                }
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                // Enforce Anomic Burner frigate/small-hull gate restriction (feat:pve.anomic-burner-missions)
                let is_burner = load_mission_meta(snap, char_id, mission_id)
                    .is_some_and(|mt| mt.is_anomic_burner)
                    || ctx.sde.missions
                        .get(&mission_id)
                        .is_some_and(|t| t.title.contains("Anomic"));
                if is_burner {
                    if let Some(ship_id) = session.ship_id {
                        if let Some(ship_item) = snap.items.get(&ship_id) {
                            if let Some(ship_type) = ctx.sde.types.get(&ship_item.type_id) {
                                // Battleship (group 27), Battlecruiser (419), Capital (30, 485, 547) rejected
                                if matches!(ship_type.group_id, 27 | 30 | 419 | 485 | 547 | 659 | 898 | 900)
                                    || ship_type.mass >= 50_000_000.0
                                {
                                    return Err(BridgeRpcError { code: "ShipRestrictedByGate".into(), message: format!("Ship {} (group {}) is too large for this Anomic Burner acceleration gate", ship_type.name, ship_type.group_id), client_error: None });
                                }
                            }
                        }
                    }
                }
                let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) else {
                    return Err(BridgeRpcError { code: "MissionNotActive".into(), message: format!("Mission {mission_id} is not active for character {char_id}"), client_error: None });
                };
                if m.state != "Accepted" {
                    return Err(BridgeRpcError { code: "InvalidMissionState".into(), message: format!("Mission {mission_id} is in state '{}', not Accepted", m.state), client_error: None });
                }
                if m.npcs_remaining > 0 {
                    return Err(BridgeRpcError { code: "RoomNpcsRemaining".into(), message: format!("Cannot activate acceleration gate while {} room NPC(s) remain", m.npcs_remaining), client_error: None });
                }
                m.current_room_id += 1;
                Ok(m.current_room_id)
            });
            match res {
                Ok(next_room) => Some(Ok(PyRep::keyval([
                    ("missionID", PyRep::Integer(mission_id as i64)),
                    ("currentRoomID", PyRep::Integer(next_room as i64)),
                    ("gateUnlocked", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("air_npe", "skip_air_npe") => handle_skip_air_npe(ctx, session),

        ("air_npe", "get_air_npe_state") | ("air_npe", "is_air_npe_enabled") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let (graph_id, curr_node, state_id, completed) = ctx.db.with_snapshot(|snap| {
                if let Some(runner) = crate::services::air_npe_nodegraph_runner::load_runner_state(snap, char_id) {
                    (
                        u32::try_from(runner.master_client_graph_id).unwrap_or(716),
                        runner.checkpoint_id,
                        runner.state_id,
                        runner.completed,
                    )
                } else if let Some(npe) = snap.npe_progress.get(&char_id) {
                    let gid = if npe.node_graph_id > 0 { npe.node_graph_id } else { 473 };
                    let st = if npe.completed { 3 } else { 0 };
                    (gid, npe.current_node_id, st, npe.completed)
                } else {
                    (473, 1, 0, false)
                }
            });
            Some(Ok(PyRep::keyval([
                ("nodeGraphID", PyRep::Integer(i64::from(graph_id))),
                ("currentNodeID", PyRep::Integer(i64::from(curr_node))),
                ("stateID", PyRep::Integer(state_id)),
                ("completed", PyRep::Bool(completed)),
                ("enabled", PyRep::Bool(true)),
            ])))
        }

        _ => None,
    }
}

fn handle_agent_mgr_attr(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&crate::router::BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let agent_id = match bound_entry
        .filter(|b| b.service == service && b.object_id > 0)
        .map(|b| b.object_id)
        .or_else(|| args.first().and_then(|a| a.as_i64()))
        .or_else(|| {
            ctx.bound_objects.read().ok().and_then(|map| {
                map.values()
                    .filter(|b| b.session_id == session.session_id && b.service == service && b.object_id > 0)
                    .max_by_key(|b| b.bound_id)
                    .map(|b| b.object_id)
            })
        }) {
        Some(id) => id,
        None => {
            return Some(Err(BridgeRpcError {
                code: "AgentNotFound".into(),
                message: "agentMgr attribute lookup requires agentID or bound agent moniker".into(),
                client_error: None,
            }));
        }
    };
    let Some(agent) = ctx.sde.agents.get(&agent_id) else {
        return Some(Err(BridgeRpcError {
            code: "UnknownAgent".into(),
            message: format!("Agent {agent_id} does not exist in SDE"),
            client_error: None,
        }));
    };
    let corp_id = agent.corporation_id;
    let faction_id = ctx.sde.npc_corporations.get(&corp_id).and_then(|c| c.faction_id).unwrap_or(0);
    let val = match method {
        "agentID" => PyRep::Integer(agent.agent_id),
        "corporationID" => PyRep::Integer(corp_id),
        "factionID" => PyRep::Integer(faction_id as i64),
        "solarsystemID" => agent.solar_system_id.map(|s| PyRep::Integer(s as i64)).unwrap_or(PyRep::None),
        "stationID" => agent.station_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        _ => PyRep::None,
    };
    Some(Ok(val))
}

fn handle_skip_air_npe(
    ctx: &ServerContext,
    session: &SessionState,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let now = ctx.now_unix();
    match crate::services::character_expert_systems::skip_air_npe_and_stop_graph(ctx, char_id, now) {
        Ok((graph_id, curr_node, completed)) => Some(Ok(PyRep::keyval([
            ("nodeGraphID", PyRep::Integer(i64::from(graph_id))),
            ("currentNodeID", PyRep::Integer(i64::from(curr_node))),
            ("completed", PyRep::Bool(completed)),
        ]))),
        Err(e) => Some(Err(e)),
    }
}

fn handle_get_disabled_missions(
    ctx: &ServerContext,
    session: &SessionState,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let disabled_ids = ctx.db.with_snapshot(|snap| {
        let mut ids: Vec<i64> = snap
            .fleet_and_social_ops
            .agent_global_disabled_missions
            .iter()
            .copied()
            .filter(|mid| ctx.sde.missions.contains_key(mid))
            .map(i64::from)
            .collect();
        if let Some(char_set) = snap
            .fleet_and_social_ops
            .agent_char_disabled_missions
            .get(&char_id)
        {
            for &mid in char_set {
                if ctx.sde.missions.contains_key(&mid) {
                    ids.push(i64::from(mid));
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter().map(PyRep::Integer).collect::<Vec<_>>()
    });
    Some(Ok(PyRep::List(disabled_ids)))
}

fn handle_get_replay_timestamp(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&crate::router::BoundObjectEntry>,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    // `eve/client/script/ui/station/agents/agents.pyj:485-491` (`GetAgentTimers`):
    // calls `self.GetAgentMoniker(agentID).GetReplayTimestamp(session.charid)` and stores
    // the returned timestamp (or `None` when the career agent is not on a replay timer).
    let (_, caller_char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let target_char_id = args
        .first()
        .and_then(|a| a.as_i64())
        .filter(|&id| id > 0)
        .unwrap_or(caller_char_id);
    let bound_agent_id = bound_entry
        .filter(|b| b.service == "agentMgr" && b.object_id > 0)
        .map(|b| b.object_id);
    let now_unix = ctx.now_unix();
    let replay_unix = ctx.db.with_snapshot(|snap| {
        if let Some(aid) = bound_agent_id {
            if let Some(&ts) = snap
                .fleet_and_social_ops
                .agent_replay_ts
                .get(&(target_char_id, aid))
                .filter(|&&ts| ts > now_unix)
            {
                return Some(ts);
            }
        }
        load_epic_arc_states(snap, target_char_id)
            .into_iter()
            .filter(|a| bound_agent_id.is_none_or(|aid| a.agent_id == aid))
            .filter_map(|a| a.cooldown_until_unix)
            .filter(|&until| until > now_unix)
            .max()
    });
    Some(Ok(replay_unix
        .map(|ts| PyRep::Integer((ts as i64) * 10_000_000 + 116_444_736_000_000_000))
        .unwrap_or(PyRep::None)))
}

#[cfg(test)]
#[path = "agents_standings_arcs_tests.rs"]
mod agents_standings_arcs_tests;

#[cfg(test)]
#[path = "agents_standings_arcs_edges_tests.rs"]
mod agents_standings_arcs_edges_tests;

