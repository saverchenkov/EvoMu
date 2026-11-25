//! `agents_missions_helpers` — Helper logic, reward formulas, and resolution for agent missions.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{MissionState, StandingTransactionRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, ClientError, PyRep};
use std::collections::HashMap;

/// Maps an internal [`eve_db::MissionInstanceStore`] invariant error into a typed [`BridgeRpcError`].
pub fn mission_store_rpc_err(err: impl std::fmt::Display) -> BridgeRpcError {
    let msg = err.to_string();
    BridgeRpcError::with_client_error("MissionStoreError", msg.clone(), ClientError::CustomInfo(msg))
}

pub fn parse_mission_id(args: &[PyRep]) -> Result<u32, BridgeRpcError> {
    let raw = args.first().and_then(|a| a.as_i64()).ok_or_else(|| BridgeRpcError {
        code: "InvalidMissionId".into(),
        message: "mission_id argument is required".into(),
        client_error: None,
    })?;
    u32::try_from(raw).map_err(|_| BridgeRpcError {
        code: "InvalidMissionId".into(),
        message: format!("mission_id {raw} is out of valid u32 range"),
        client_error: None,
    })
}

pub fn is_storyline_template(tmpl: &eve_sde::MissionTemplate) -> bool {
    if tmpl
        .content_template
        .as_deref()
        .is_some_and(|ct| ct.contains("Storyline"))
    {
        return true;
    }
    let t = tmpl.title.to_lowercase();
    t.contains("storyline") || t.contains("important") || t.contains("materials for war")
}

pub fn is_storyline_agent_type(agent_type_id: u32) -> bool {
    matches!(agent_type_id, 6 | 7)
}

pub fn is_unsupported_direct_agent_type(agent_type_id: u32) -> bool {
    matches!(agent_type_id, 11 | 12)
}

pub fn is_rnd_agent(agent: &eve_sde::AgentInfo) -> bool {
    agent.agent_type_id == 4
}

pub fn is_fw_agent(agent: &eve_sde::AgentInfo) -> bool {
    matches!(
        agent.corporation_id,
        1000121 | 1000147 | 1000085 | 1000009 | 1000179 | 1000180 | 1000181 | 1000182
    )
}

pub fn rnd_tritanium_required_qty(agent_level: u8) -> i32 {
    // Scaled by agent level: Level 1: 300, Level 2: 900, Level 3: 2700, Level 4: 8100, Level 5: 24300
    let lvl = agent_level.clamp(1, 5) as u32;
    300 * 3_i32.pow(lvl - 1)
}

pub fn mission_bonus_duration(agent_level: u8) -> u64 {
    // Bonus timer of 10 min (600s) to 6 h (21,600s)
    let dur = match agent_level {
        1 => 1_800,  // 30 min
        2 => 3_600,  // 1 hour
        3 => 7_200,  // 2 hours
        4 => 14_400, // 4 hours
        _ => 21_600, // 6 hours
    };
    dur.clamp(600, 21_600)
}

pub fn mission_matches_agent_pool(
    tmpl: &eve_sde::MissionTemplate,
    agent: &eve_sde::AgentInfo,
) -> bool {
    if is_storyline_template(tmpl) {
        return false;
    }
    let ct = tmpl.content_template.as_deref().unwrap_or_default();
    if ct.contains("EpicArc") && agent.agent_type_id != 10 {
        return false;
    }
    let title_lc = tmpl.title.to_lowercase();
    if (title_lc.contains("anomic") || title_lc.contains("burner")) && agent.level < 4 {
        return false;
    }
    match agent.agent_type_id {
        10 => ct.contains("EpicArc"),
        4 => ct.contains("Research"),
        13 => ct.contains("Heraldry") || ct.contains("BasicKill"),
        12 => ct.contains("BasicKill") || ct.contains("BasicCourier") || ct.contains("BasicMining"),
        5 | 8 => {
            ct.contains("BasicKill") || ct.contains("BasicCourier") || ct.contains("BasicTrade")
        }
        11 => false,
        _ => match agent.division_id {
            18 => ct.contains("Research") || tmpl.is_courier,
            22 => ct.contains("BasicCourier"),
            23 => ct.contains("BasicMining") || title_lc.contains("mining"),
            24 => ct.contains("BasicKill"),
            37 => ct.contains("Heraldry") || ct.contains("BasicKill"),
            _ => ct.contains("BasicKill") || ct.contains("BasicCourier"),
        },
    }
}

/// Level- and dungeon-difficulty-aware variant of [`mission_matches_agent_pool`].
/// Enforces `sde.dungeons[dungeon_id].difficulty == agent.level` for dungeon-backed
/// regular missions, while permitting L4+ agents (`agent.level >= 4`) to draw difficulty-5
/// `"Anomic"` / `"Burner"` missions.
pub fn mission_matches_agent_pool_with_sde(
    sde: &eve_sde::SdeRegistry,
    tmpl: &eve_sde::MissionTemplate,
    agent: &eve_sde::AgentInfo,
) -> bool {
    if !mission_matches_agent_pool(tmpl, agent) {
        return false;
    }
    let title_lc = tmpl.title.to_lowercase();
    if title_lc.contains("anomic") || title_lc.contains("burner") {
        return agent.level >= 4;
    }
    let dungeon_diff = tmpl
        .dungeon_id
        .and_then(|did| sde.dungeons.get(&did))
        .and_then(|d| d.difficulty)
        .and_then(|d| u8::try_from(d).ok())
        .filter(|&d| (1..=5).contains(&d));
    dungeon_diff.is_none_or(|diff| diff == agent.level.clamp(1, 5))
}

pub fn resolve_fw_warzone_system(agent_corp_id: i64) -> u32 {
    // Selects a canonical FW warzone system appropriate for militia
    match agent_corp_id {
        1000121 | 1000181 => 30002062, // Federal Defense Union -> Onnamon
        1000147 | 1000179 => 30002097, // 24th Imperial Crusade -> Mehatoor
        1000085 | 1000182 => 30002098, // Tribal Liberation Force -> Amo
        1000009 | 1000180 => 30002061, // State Protectorate -> Pynekoh
        _ => 30002062,
    }
}

pub fn resolve_agent_and_corp_for_system(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    station_id: Option<i64>,
    is_storyline: bool,
) -> (i64, i64) {
    let preferred_station = station_id.or_else(|| {
        ctx.db
            .get_character(char_id)
            .ok()
            .map(|ch| ch.home_station_id)
    });
    resolve_agent_and_corp_for_system_with_preferred_station(
        ctx,
        sys_id,
        preferred_station,
        is_storyline,
    )
}

pub fn resolve_agent_and_corp_for_system_with_preferred_station(
    ctx: &ServerContext,
    sys_id: u32,
    preferred_station: Option<i64>,
    is_storyline: bool,
) -> (i64, i64) {
    let cur_sys = ctx.sde.solar_systems.get(&sys_id);
    let cur_constellation = cur_sys.map(|s| s.constellation_id);
    let cur_region = cur_sys.map(|s| s.region_id);
    let primary_corp = cur_sys
        .and_then(|s| {
            preferred_station
                .and_then(|sid| s.stations.iter().find(|st| st.station_id == sid))
                .or_else(|| s.stations.first())
        })
        .map(|st| st.owner_corporation_id)
        .or_else(|| ctx.sde.npc_corporations.keys().copied().min())
        .unwrap_or(0);

    let agent = if is_storyline {
        ctx.sde
            .agents
            .values()
            .filter(|a| a.solar_system_id == Some(sys_id) && is_storyline_agent_type(a.agent_type_id))
            .min_by_key(|a| a.agent_id)
            .or_else(|| {
                ctx.sde.agents.values().filter(|a| {
                    a.corporation_id == primary_corp && is_storyline_agent_type(a.agent_type_id)
                }).min_by_key(|a| a.agent_id)
            })
            .or_else(|| {
                ctx.sde.agents.values().filter(|a| {
                    is_storyline_agent_type(a.agent_type_id)
                        && a.solar_system_id.and_then(|s| ctx.sde.solar_systems.get(&s)).map(|s| s.constellation_id)
                            == cur_constellation
                }).min_by_key(|a| a.agent_id)
            })
            .or_else(|| {
                ctx.sde.agents.values().filter(|a| {
                    is_storyline_agent_type(a.agent_type_id)
                        && a.solar_system_id.and_then(|s| ctx.sde.solar_systems.get(&s)).map(|s| s.region_id)
                            == cur_region
                }).min_by_key(|a| a.agent_id)
            })
    } else {
        let is_eligible_regular = |a: &eve_sde::AgentInfo| -> bool {
            !is_storyline_agent_type(a.agent_type_id)
                && !is_unsupported_direct_agent_type(a.agent_type_id)
        };
        preferred_station
            .and_then(|sid| {
                ctx.sde.agents.values().filter(|a| {
                    a.solar_system_id == Some(sys_id)
                        && a.station_id == Some(sid)
                        && is_eligible_regular(a)
                }).min_by_key(|a| (a.level, a.agent_id))
            })
            .or_else(|| {
                ctx.sde.agents.values().filter(|a| {
                    a.solar_system_id == Some(sys_id)
                        && a.corporation_id == primary_corp
                        && is_eligible_regular(a)
                }).min_by_key(|a| (a.level, a.agent_id))
            })
            .or_else(|| {
                ctx.sde.agents
                    .values()
                    .filter(|a| a.solar_system_id == Some(sys_id) && is_eligible_regular(a))
                    .min_by_key(|a| (a.level, a.agent_id))
            })
    };

    match agent {
        Some(a) => (a.agent_id, a.corporation_id),
        None => (0, primary_corp),
    }
}

pub fn resolve_mission_npc_type(ctx: &ServerContext) -> u32 {
    let is_l1_frigate_rat = |inv: &eve_sde::InvType| -> bool {
        if !inv
            .default_attributes
            .contains_key(&eve_sde::attr_ids::ENTITY_KILL_BOUNTY)
            || inv.radius < 25.0
        {
            return false;
        }
        let shield = inv
            .default_attributes
            .get(&eve_sde::attr_ids::SHIELD_CAPACITY)
            .copied()
            .unwrap_or(0.0);
        let armor = inv
            .default_attributes
            .get(&eve_sde::attr_ids::ARMOR_HP)
            .copied()
            .unwrap_or(0.0);
        let hull = inv
            .default_attributes
            .get(&eve_sde::attr_ids::HP_STRUCT)
            .copied()
            .unwrap_or(0.0);
        (50.0..=450.0).contains(&(shield + armor + hull))
    };

    let mut group_ids: Vec<u32> = ctx
        .sde
        .npc_spawn_groups
        .values()
        .filter(|sg| sg.ship_class == 1)
        .map(|sg| sg.group_id)
        .collect();
    group_ids.sort_unstable();
    for gid in group_ids {
        if let Some(sg) = ctx.sde.npc_spawn_groups.get(&gid) {
            for &tid in &sg.npc_type_ids {
                if let Some(inv) = ctx.sde.types.get(&tid) {
                    if is_l1_frigate_rat(inv) {
                        return tid;
                    }
                }
            }
        }
    }
    ctx.sde
        .types
        .values()
        .filter(|t| t.category_id == 11 && is_l1_frigate_rat(t))
        .map(|t| t.type_id)
        .min()
        .unwrap_or(0)
}

/// Resolves the Connections skill (`Mining Connections = 3893`, `Distribution Connections = 3894`,
/// `Security Connections = 3895`) for an NPC corporation division using V24 SDE division metadata.
pub fn resolve_division_connections_skill(
    ctx: &ServerContext,
    division_id: u32,
) -> Option<eve_db::TypeId> {
    if let Ok(divisions) = ctx.sde.load_npc_corporation_divisions() {
        if let Some(div) = divisions.values().find(|d| d.division_id == division_id) {
            let lower = div.internal_name.to_lowercase();
            if lower.contains("distribution") {
                return Some(3894);
            }
            if lower.contains("mining") {
                return Some(3893);
            }
            if lower.contains("security") {
                return Some(3895);
            }
        }
    }
    match division_id {
        22 | 27 => Some(3894),                    // Distribution Connections
        23 => Some(3893),                         // Mining Connections
        24 | 20 | 21 | 25 | 26 | 28 => Some(3895), // Security Connections
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CharacterMissionRewards {
    pub isk_reward: i64,
    pub lp_reward: i64,
    pub bonus_reward: i64,
}

/// Origin of a mission's base `(ISK, bonus ISK, bonus window, base LP)` definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewardCatalogSource {
    /// Directly calibrated from documented EVE University Wiki mission reward reports.
    WikiCatalog,
    /// Real SDE Heraldry (`Paragon`) LP columns paired with a server-calibrated stipend.
    SdeHeraldryAndDerivedStipend,
    /// Server-calibrated multi-signal reward curve when FSD keeps CCP reward columns server-side.
    DerivedMissionCurve,
}

/// Base (unskilled, security-unadjusted) reward catalog definition for a single mission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissionRewardDefinition {
    pub base_isk: i64,
    pub time_bonus_isk: i64,
    pub time_bonus_window_secs: u64,
    pub base_lp: i64,
    pub base_lp_alpha: i64,
    pub grants_lp: bool,
    pub source: RewardCatalogSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MissionRewardCategory {
    Security,
    Courier,
    Mining,
    Storyline,
    EpicArc,
    Research,
    Career,
    DataCenter,
    Cosmos,
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn stable_mission_seed(mission_id: u32, norm_title: &str) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325 ^ u64::from(mission_id);
    for &b in norm_title.as_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01B3);
    }
    splitmix64(h ^ (u64::from(mission_id) << 32))
}

fn normalized_mission_title(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn round_isk_to(amount: i64, step: i64) -> i64 {
    if step <= 1 {
        return amount.max(1);
    }
    (((amount + step / 2) / step) * step).max(step)
}

fn parse_chain_step_ratio(norm_title: &str) -> Option<f64> {
    let start = norm_title.rfind('(')?;
    let end = norm_title[start..].find(')')? + start;
    let inner = norm_title[start + 1..end].trim();
    let (left, right) = inner.split_once(" of ")?;
    let step: u32 = left.trim().parse().ok()?;
    let total: u32 = right.trim().parse().ok()?;
    if total == 0 || step == 0 || step > total {
        return None;
    }
    Some(f64::from(step) / f64::from(total))
}

/// Curated per-mission reward definitions sourced from documented EVE University Wiki mission reports
/// (`wiki:Denial_Of_Reconnaissance`, `wiki:Interstellar_Railroad`, `wiki:Patriotic_Measures`,
/// `wiki:Foreign_Investment`, `wiki:Serpentis_Extravaganza_(Level_3)`, `wiki:The_Sansha_Spies_(Level_4)`,
/// `wiki:Desperate_Maneuvers`, Storyline reports, and SOE / Empire Epic Arc reports) verified against
/// real `sde_missions` IDs and titles.
fn wiki_mission_reward_override(
    mission_id: u32,
    norm_title: &str,
    effective_lvl: u8,
) -> Option<MissionRewardDefinition> {
    let wiki = |lvl: u8, base_isk: i64, time_bonus_isk: i64, win_s: u64, base_lp: i64| {
        (
            Some(lvl),
            MissionRewardDefinition {
                base_isk,
                time_bonus_isk,
                time_bonus_window_secs: win_s.clamp(600, 21_600),
                base_lp,
                base_lp_alpha: if base_lp > 0 { (base_lp / 2).max(1) } else { 0 },
                grants_lp: base_lp > 0,
                source: RewardCatalogSource::WikiCatalog,
            },
        )
    };
    let no_lp_wiki = |base_isk: i64, time_bonus_isk: i64, win_s: u64| {
        (
            None::<u8>,
            MissionRewardDefinition {
                base_isk,
                time_bonus_isk,
                time_bonus_window_secs: win_s.clamp(600, 21_600),
                base_lp: 0,
                base_lp_alpha: 0,
                grants_lp: false,
                source: RewardCatalogSource::WikiCatalog,
            },
        )
    };

    let (canon_lvl, def) = match (mission_id, norm_title) {
        // Regular L1-L4 Security missions (`BasicKillMission`)
        (13777, "denial of reconnaissance") => wiki(1, 67_000, 64_000, 3_600, 73),
        (13492, "interstellar railroad (3 of 4)") => wiki(1, 14_000, 9_000, 3_600, 14),
        (13493, "interstellar railroad (4 of 4)") => wiki(1, 15_000, 16_000, 3_600, 19),
        (13514, "patriotic measures (4 of 5)") => wiki(1, 29_000, 37_000, 3_840, 35),
        (4886, "foreign investment") => wiki(2, 82_000, 75_000, 5_400, 113),
        (1140, "serpentis extravaganza") => wiki(3, 692_000, 799_000, 10_800, 1_829),
        (900, "the sansha spies") => wiki(4, 358_000, 510_000, 14_400, 1_979),
        (13741, "desperate maneuvers") => wiki(4, 2_600_000, 2_800_000, 21_600, 13_202),
        // Regular L1 Courier missions (`BasicCourierMission`)
        (13491, "interstellar railroad (2 of 4)") => wiki(1, 20_000, 20_000, 3_600, 21),
        (13512, "patriotic measures (2 of 5)") => wiki(1, 24_000, 25_000, 2_640, 23),
        // Storyline missions (`grants_lp = false` per `wiki:Loyalty_Points`)
        (1872, "the audesder war effort, hand in the voucher (1 of 8)") => {
            no_lp_wiki(298_000, 289_000, 2_880)
        }
        (2056, "civilian pullout") => no_lp_wiki(730_000, 761_000, 14_400),
        (1900, "civilian pullout - more civilians to evacuate") => {
            no_lp_wiki(759_000, 802_000, 14_340)
        }
        (2825, "courier gone missing (1 of 2)") => no_lp_wiki(15_000_000, 10_000_000, 10_800),
        (2570, "heads of management (1 of 2)") => no_lp_wiki(10_000_000, 6_000_000, 21_600),
        (15219, "soothe the salvage beast") => no_lp_wiki(1_950_000, 2_300_000, 14_700),
        // Epic Arc missions (`grants_lp = false` per `wiki:Loyalty_Points`)
        (14117, "a beacon beckons") => no_lp_wiki(100_000, 169_000, 3_600),
        (14118, "agent inquiry") => no_lp_wiki(100_000, 178_500, 3_600),
        (14135, "an economy under threat") => no_lp_wiki(100_000, 131_000, 3_600),
        (14390, "burning down the hive") => no_lp_wiki(150_000, 210_500, 3_600),
        (14406, "a call to trial") => no_lp_wiki(200_000, 39_000, 3_600),
        (14511, "an honorable betrayal") => no_lp_wiki(300_000, 1_000_000, 21_600),
        (14597, "almost unmasked") => no_lp_wiki(300_000, 1_500_000, 21_600),
        (14607, "a general's best friend") => no_lp_wiki(5_000_000, 1_000_000, 21_600),
        (14821, "a different kind of director") => no_lp_wiki(5_000_000, 1_000_000, 21_600),
        (14851, "a demonstration") => no_lp_wiki(6_000_000, 1_000_000, 21_600),
        (14946, "carry on") => no_lp_wiki(10_000_000, 1_000_000, 21_600),
        _ => return None,
    };
    if canon_lvl.is_some_and(|lvl| effective_lvl != lvl) {
        return None;
    }
    Some(def)
}

/// DERIVED: Server-calibrated Anomic Burner mission rewards anchored to L4 Burner dungeon
/// difficulty 5 and the `wiki:Anomic_missions` bounty tiers (Agent/Team 5M bounty tier vs Base 13M-22M tier),
/// since FSD omits CCP server ISK/LP/bonus columns.
fn derived_anomic_burner_rewards(
    mission_id: u32,
    norm_title: &str,
    effective_lvl: u8,
) -> Option<MissionRewardDefinition> {
    if effective_lvl < 4 {
        return None;
    }
    let mk = |base_isk: i64, time_bonus_isk: i64, win_s: u64, base_lp: i64| {
        MissionRewardDefinition {
            base_isk,
            time_bonus_isk,
            time_bonus_window_secs: win_s,
            base_lp,
            base_lp_alpha: (base_lp / 2).max(1),
            grants_lp: true,
            source: RewardCatalogSource::DerivedMissionCurve,
        }
    };
    let named = match mission_id {
        // Anomic Agent (pirate frigate pockets)
        16087 if norm_title == "anomic agent" => Some(mk(3_350_000, 1_920_000, 7_200, 8_750)),
        16089 if norm_title == "anomic agent" => Some(mk(2_950_000, 1_680_000, 7_200, 7_900)),
        16090 if norm_title == "anomic agent" => Some(mk(3_200_000, 1_840_000, 7_200, 8_400)),
        16091 if norm_title == "anomic agent" => Some(mk(3_050_000, 1_760_000, 7_200, 8_150)),
        16092 if norm_title == "anomic agent" => Some(mk(3_450_000, 1_980_000, 7_200, 8_900)),
        // Anomic Team (assault frigate + logi pockets)
        16093 if norm_title == "anomic team" => Some(mk(3_400_000, 1_950_000, 7_200, 9_000)),
        16094 if norm_title == "anomic team" => Some(mk(3_550_000, 2_050_000, 7_200, 9_300)),
        16095 if norm_title == "anomic team" => Some(mk(3_680_000, 2_120_000, 7_200, 9_550)),
        16096 if norm_title == "anomic team" => Some(mk(3_820_000, 2_210_000, 7_200, 9_800)),
        // Anomic Base (cruiser/battlecruiser burner bases)
        16152 if norm_title == "anomic base" => Some(mk(5_600_000, 2_850_000, 10_800, 13_500)),
        16153 if norm_title == "anomic base" => Some(mk(4_900_000, 2_450_000, 10_800, 12_000)),
        16154 if norm_title == "anomic base" => Some(mk(6_200_000, 3_150_000, 10_800, 14_500)),
        16168 if norm_title == "anomic base" => Some(mk(4_400_000, 2_250_000, 10_800, 11_000)),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    if norm_title.contains("anomic") || norm_title.contains("burner") {
        let seed = stable_mission_seed(mission_id, norm_title);
        let (base_isk, base_lp, win_s) = if norm_title.contains("base") {
            let isk = round_isk_to(4_300_000 + ((seed % 20) as i64) * 95_000, 1_000);
            let lp = 10_800 + ((splitmix64(seed ^ 17) % 38) as i64) * 100;
            (isk, lp, 10_800_u64)
        } else {
            let isk = round_isk_to(2_950_000 + ((seed % 18) as i64) * 50_000, 1_000);
            let lp = 7_800 + ((splitmix64(seed ^ 17) % 21) as i64) * 100;
            (isk, lp, 7_200_u64)
        };
        let bonus_pct = 54 + ((seed >> 12) % 16) as i64;
        return Some(mk(
            base_isk,
            round_isk_to(base_isk * bonus_pct / 100, 1_000),
            win_s,
            base_lp,
        ));
    }
    None
}

/// DERIVED: Combines real SDE Heraldry (`Paragon`) LP columns (`lp_omega`, `lp_alpha`) with a
/// server-calibrated hull-tier ISK stipend and bonus timer since FSD omits Paragon ISK columns.
fn resolve_heraldry_reward_definition(
    mission_id: u32,
    tmpl: &eve_sde::MissionTemplate,
    norm_title: &str,
) -> MissionRewardDefinition {
    let seed = stable_mission_seed(mission_id, norm_title);
    let base_lp = if tmpl.lp_omega > 0 { tmpl.lp_omega } else { 2_000 };
    let base_lp_alpha = if tmpl.lp_alpha > 0 {
        tmpl.lp_alpha
    } else {
        (base_lp / 10).max(1)
    };
    let lp_span = (base_lp - 1_500).clamp(0, 3_500);
    let base_isk = round_isk_to(115_000 + lp_span * 28 + ((seed % 17) as i64) * 2_500, 500);
    let bonus_pct = 62 + ((seed >> 10) % 33) as i64;
    let time_bonus_isk = round_isk_to(base_isk * bonus_pct / 100, 500);
    let win_mins = 40 + ((seed >> 20) % 45);
    MissionRewardDefinition {
        base_isk,
        time_bonus_isk,
        time_bonus_window_secs: (win_mins * 60).clamp(1_800, 5_400),
        base_lp,
        base_lp_alpha,
        grants_lp: true,
        source: RewardCatalogSource::SdeHeraldryAndDerivedStipend,
    }
}

fn classify_reward_category(
    tmpl: Option<&eve_sde::MissionTemplate>,
    norm_title: &str,
    agent_info: Option<&eve_sde::AgentInfo>,
) -> MissionRewardCategory {
    let ct = tmpl
        .and_then(|t| t.content_template.as_deref())
        .unwrap_or_default();
    if ct.contains("EpicArc") || agent_info.is_some_and(|a| a.agent_type_id == 10) {
        return MissionRewardCategory::EpicArc;
    }
    if tmpl.is_some_and(is_storyline_template)
        || agent_info.is_some_and(|a| is_storyline_agent_type(a.agent_type_id))
    {
        return MissionRewardCategory::Storyline;
    }
    if agent_info.is_some_and(|a| a.agent_type_id == 12) {
        return MissionRewardCategory::Career;
    }
    if agent_info.is_some_and(|a| a.agent_type_id == 5) {
        return MissionRewardCategory::DataCenter;
    }
    if agent_info.is_some_and(|a| a.agent_type_id == 8) {
        return MissionRewardCategory::Cosmos;
    }
    if ct.contains("Research") || agent_info.is_some_and(is_rnd_agent) {
        return MissionRewardCategory::Research;
    }
    if ct.contains("Mining")
        || norm_title.contains("mining")
        || (!ct.contains("Kill") && agent_info.is_some_and(|a| a.division_id == 23))
    {
        return MissionRewardCategory::Mining;
    }
    if ct.contains("Trade")
        || ct.contains("Courier")
        || tmpl.is_some_and(|t| t.is_courier)
        || (!ct.contains("Kill") && agent_info.is_some_and(|a| matches!(a.division_id, 22 | 27)))
    {
        return MissionRewardCategory::Courier;
    }
    MissionRewardCategory::Security
}

fn lerp_i64(lo: i64, hi: i64, unit_u64: u64) -> i64 {
    let t = (unit_u64 % 10_000) as f64 / 9_999.0;
    lo + (((hi - lo) as f64) * t).round() as i64
}

/// DERIVED: Server-calibrated per-mission reward curve calibrated to documented EVE mission ranges by
/// `(category, effective_lvl, dungeon_difficulty, chain_step)` using independent per-mission hash lanes
/// for ISK, LP, bonus ratio (intentionally never fixed at 0.50), and bonus timer window (`[600, 21_600]`s,
/// keeping L1 windows strictly `< 7_200`s).
fn derive_mission_reward_definition(
    mission_id: u32,
    tmpl: Option<&eve_sde::MissionTemplate>,
    norm_title: &str,
    category: MissionRewardCategory,
    effective_lvl: u8,
    dungeon_diff: Option<u8>,
    dungeon_arch: Option<u32>,
) -> MissionRewardDefinition {
    let idx = usize::from(effective_lvl.clamp(1, 5)) - 1;
    let (isk_lo, isk_hi, lp_lo, lp_hi, win_lo_m, win_hi_m) = match category {
        MissionRewardCategory::Security => [
            (16_000_i64, 70_000_i64, 18_i64, 92_i64, 25_u64, 75_u64),
            (68_000, 190_000, 88, 310, 45, 110),
            (260_000, 780_000, 540, 2_050, 90, 210),
            (780_000, 2_650_000, 2_250, 8_400, 180, 360),
            (2_400_000, 6_400_000, 8_200, 22_000, 240, 360),
        ][idx],
        MissionRewardCategory::Courier => [
            (18_000, 46_000, 18, 58, 20, 65),
            (54_000, 142_000, 68, 195, 35, 90),
            (180_000, 460_000, 340, 940, 60, 150),
            (540_000, 1_420_000, 1_150, 3_650, 90, 240),
            (1_250_000, 2_750_000, 3_200, 8_200, 120, 300),
        ][idx],
        MissionRewardCategory::Mining => [
            (20_000, 54_000, 22, 72, 25, 75),
            (62_000, 165_000, 84, 245, 45, 100),
            (220_000, 540_000, 440, 1_220, 75, 180),
            (640_000, 1_680_000, 1_520, 4_450, 120, 270),
            (1_450_000, 3_100_000, 4_100, 10_200, 180, 360),
        ][idx],
        MissionRewardCategory::Storyline => [
            (75_000, 185_000, 0, 0, 30, 80),
            (190_000, 480_000, 0, 0, 60, 150),
            (480_000, 1_250_000, 0, 0, 120, 240),
            (1_250_000, 3_600_000, 0, 0, 180, 360),
            (3_100_000, 6_500_000, 0, 0, 240, 360),
        ][idx],
        MissionRewardCategory::EpicArc => [
            (95_000, 235_000, 0, 0, 30, 80),
            (245_000, 610_000, 0, 0, 60, 150),
            (650_000, 1_700_000, 0, 0, 120, 240),
            (1_550_000, 5_200_000, 0, 0, 180, 360),
            (3_500_000, 8_500_000, 0, 0, 240, 360),
        ][idx],
        MissionRewardCategory::Career => (38_000, 92_000, 0, 0, 30, 60),
        MissionRewardCategory::DataCenter => [
            (18_000, 48_000, 0, 0, 30, 60),
            (45_000, 120_000, 0, 0, 30, 90),
            (95_000, 260_000, 0, 0, 60, 120),
            (220_000, 620_000, 0, 0, 60, 180),
            (480_000, 1_150_000, 0, 0, 90, 240),
        ][idx],
        MissionRewardCategory::Cosmos => [
            (55_000, 145_000, 0, 0, 30, 80),
            (160_000, 420_000, 0, 0, 60, 150),
            (420_000, 1_100_000, 0, 0, 120, 240),
            (1_050_000, 2_900_000, 0, 0, 180, 360),
            (2_500_000, 5_600_000, 0, 0, 240, 360),
        ][idx],
        // R&D daily missions double daily Research Points for 1 day and award 0 ISK / 0 LP.
        MissionRewardCategory::Research => (0, 0, 0, 0, 30, 90),
    };

    let seed = stable_mission_seed(mission_id, norm_title)
        ^ u64::from(dungeon_arch.unwrap_or(0)).wrapping_mul(0x9E37_79B1);
    let h_isk = splitmix64(seed ^ 0x9E37_79B9_7F4A_7C15);
    let h_lp = splitmix64(seed ^ 0xBF58_476D_1CE4_E5B9);
    let h_ratio = splitmix64(seed ^ 0x94D0_49BB_1331_11EB);
    let h_win = splitmix64(seed ^ 0xD6E8_FEB8_6659_FD93);

    // Subtle chain-step & dungeon-difficulty weighting keeps multi-part finales and harder dungeons distinct.
    let chain_mult = parse_chain_step_ratio(norm_title).map_or(1.0, |r| 0.90 + 0.25 * r);
    let diff_mult = match dungeon_diff {
        Some(d) if d > effective_lvl => 1.12,
        Some(d) if d < effective_lvl => 0.92,
        _ => 1.0,
    };

    let (base_isk, time_bonus_isk) = if isk_hi <= 0 {
        (0, 0)
    } else {
        let step = if isk_hi <= 100_000 { 500 } else { 1_000 };
        let raw_isk =
            ((lerp_i64(isk_lo, isk_hi, h_isk) as f64) * chain_mult * diff_mult).round() as i64;
        let base = round_isk_to(raw_isk.clamp(isk_lo, isk_hi.saturating_mul(5) / 4), step);

        // Bonus ratio spans [64%, 128%] (never flat 50%).
        let bonus_pct = 64 + (h_ratio % 65) as i64;
        let bonus = round_isk_to(base * bonus_pct / 100, step);
        (base, bonus)
    };

    let win_span = (win_hi_m - win_lo_m).max(1);
    let win_mins = win_lo_m + (h_win % (win_span + 1));
    let max_win_secs = if effective_lvl == 1 || category == MissionRewardCategory::Career {
        5_400
    } else {
        21_600
    };
    let time_bonus_window_secs = (win_mins * 60).clamp(600, max_win_secs);

    let grants_lp = lp_hi > 0;
    let (base_lp, base_lp_alpha) = if !grants_lp {
        (0, 0)
    } else if let Some(t) = tmpl.filter(|t| t.lp_omega > 0) {
        let alpha = if t.lp_alpha > 0 {
            t.lp_alpha
        } else {
            (t.lp_omega / 2).max(1)
        };
        (t.lp_omega, alpha)
    } else {
        let raw_lp = ((lerp_i64(lp_lo, lp_hi, h_lp) as f64) * chain_mult * diff_mult).round() as i64;
        let omega = raw_lp.clamp(lp_lo, lp_hi.saturating_mul(5) / 4).max(1);
        (omega, (omega / 2).max(1))
    };

    MissionRewardDefinition {
        base_isk,
        time_bonus_isk,
        time_bonus_window_secs,
        base_lp,
        base_lp_alpha,
        grants_lp,
        source: RewardCatalogSource::DerivedMissionCurve,
    }
}

/// Resolves the per-mission base `(ISK, time_bonus_isk, time_bonus_window_secs, base_lp)` definition
/// from curated EVE Wiki entries, real SDE Heraldry LP columns, or calibrated multi-signal curves.
pub fn resolve_mission_reward_definition(
    ctx: &ServerContext,
    char_id: i64,
    agent_id: i64,
    mission_id: u32,
) -> MissionRewardDefinition {
    let tmpl = ctx.sde.missions.get(&mission_id);
    let agent_info = ctx.sde.agents.get(&agent_id);
    let offered_lvl = ctx.db.with_snapshot(|snap| {
        snap.missions
            .get_by_character_template(char_id, mission_id)
            .and_then(|m| m.offered_level)
    });
    let dungeon = tmpl
        .and_then(|t| t.dungeon_id)
        .and_then(|did| ctx.sde.dungeons.get(&did));
    let dungeon_diff = dungeon
        .and_then(|d| d.difficulty)
        .and_then(|d| u8::try_from(d).ok())
        .filter(|&d| (1..=5).contains(&d));
    let is_storyline_or_epic = agent_info
        .is_some_and(|a| is_storyline_agent_type(a.agent_type_id) || a.agent_type_id == 10)
        || tmpl.is_some_and(|t| {
            is_storyline_template(t)
                || t.content_template
                    .as_deref()
                    .is_some_and(|ct| ct.contains("EpicArc"))
        });
    let effective_lvl = if is_storyline_or_epic {
        offered_lvl
            .or(dungeon_diff)
            .or_else(|| agent_info.map(|a| a.level))
            .unwrap_or(1)
            .clamp(1, 5)
    } else {
        match (offered_lvl, agent_info.map(|a| a.level)) {
            (Some(ol), Some(al)) => ol.min(al).clamp(1, 5),
            (Some(ol), None) => ol.clamp(1, 5),
            (None, Some(al)) => al.clamp(1, 5),
            (None, None) => dungeon_diff.unwrap_or(1).clamp(1, 5),
        }
    };

    let norm_title = tmpl
        .map(|t| normalized_mission_title(&t.title))
        .unwrap_or_default();
    let is_heraldry = tmpl.is_some_and(|t| {
        t.content_template
            .as_deref()
            .is_some_and(|ct| ct.contains("Heraldry"))
    }) || agent_info.is_some_and(|a| a.agent_type_id == 13);

    let mut def = if let Some(wiki_def) =
        wiki_mission_reward_override(mission_id, &norm_title, effective_lvl)
    {
        let mut out = wiki_def;
        if let Some(t) = tmpl.filter(|t| t.lp_omega > 0 && out.grants_lp) {
            out.base_lp = t.lp_omega;
            out.base_lp_alpha = if t.lp_alpha > 0 {
                t.lp_alpha
            } else {
                (t.lp_omega / 2).max(1)
            };
        }
        out
    } else if is_heraldry {
        if let Some(t) = tmpl {
            resolve_heraldry_reward_definition(mission_id, t, &norm_title)
        } else {
            derive_mission_reward_definition(
                mission_id,
                None,
                &norm_title,
                MissionRewardCategory::Courier,
                effective_lvl,
                dungeon_diff,
                dungeon.and_then(|d| d.archetype_id),
            )
        }
    } else if let Some(anomic_def) =
        derived_anomic_burner_rewards(mission_id, &norm_title, effective_lvl)
    {
        let mut out = anomic_def;
        if let Some(t) = tmpl.filter(|t| t.lp_omega > 0) {
            out.base_lp = t.lp_omega;
            out.base_lp_alpha = if t.lp_alpha > 0 {
                t.lp_alpha
            } else {
                (t.lp_omega / 2).max(1)
            };
        }
        out
    } else {
        let category = classify_reward_category(tmpl, &norm_title, agent_info);
        let capped_diff = dungeon_diff.map(|d| d.min(effective_lvl));
        derive_mission_reward_definition(
            mission_id,
            tmpl,
            &norm_title,
            category,
            effective_lvl,
            capped_diff,
            dungeon.and_then(|d| d.archetype_id),
        )
    };

    // Data Center (5), COSMOS (8), Epic Arc (10), Tutorial (11), Career (12), R&D, and Storyline agents pay 0 LP.
    let is_rnd_mission = agent_info.is_some_and(is_rnd_agent)
        || tmpl.is_some_and(|t| {
            t.content_template
                .as_deref()
                .is_some_and(|ct| ct.contains("Research"))
        });
    let no_lp_agent = agent_info.is_some_and(|a| {
        is_rnd_agent(a)
            || is_storyline_agent_type(a.agent_type_id)
            || matches!(a.agent_type_id, 5 | 8 | 10..=12)
    });
    let no_lp_tmpl = tmpl.is_some_and(|t| {
        is_storyline_template(t)
            || t.content_template
                .as_deref()
                .is_some_and(|ct| ct.contains("EpicArc"))
    });
    if is_rnd_mission {
        def.base_isk = 0;
        def.time_bonus_isk = 0;
    }
    if is_rnd_mission || no_lp_agent || no_lp_tmpl {
        def.grants_lp = false;
        def.base_lp = 0;
        def.base_lp_alpha = 0;
    }

    def
}

/// Returns the per-mission bonus completion window in seconds (`[600, 21_600]`).
pub fn mission_bonus_window_secs(
    ctx: &ServerContext,
    char_id: i64,
    agent_id: i64,
    mission_id: u32,
) -> u64 {
    resolve_mission_reward_definition(ctx, char_id, agent_id, mission_id)
        .time_bonus_window_secs
        .clamp(600, 21_600)
}

/// Computes mission rewards `(isk_reward, lp_reward, bonus_reward)` from per-mission reward
/// definitions (`wiki:Missions`, `wiki:Loyalty_Points`), agent solar-system true security
/// (`1.6288 - true_sec`), and character Connections/Negotiation skills
/// (`feat:pve.mission-lp-isk-reward-formula`, `feat:pve.mission-complete`, `plan:E-5`).
pub fn compute_character_mission_rewards(
    ctx: &ServerContext,
    char_id: i64,
    agent_id: i64,
    mission_id: u32,
    sys_id: u32,
) -> CharacterMissionRewards {
    let def = resolve_mission_reward_definition(ctx, char_id, agent_id, mission_id);
    let agent_info = ctx.sde.agents.get(&agent_id);
    let agent_sys_id = agent_info.and_then(|a| a.solar_system_id).unwrap_or(sys_id);
    let true_sec = ctx
        .sde
        .solar_systems
        .get(&agent_sys_id)
        .map(|s| s.security_status)
        .unwrap_or(1.0);

    let conn_skill_id =
        agent_info.and_then(|a| resolve_division_connections_skill(ctx, a.division_id));

    let (is_alpha, neg_lvl, conn_lvl) = ctx.db.with_snapshot(|snap| {
        let alpha = snap
            .characters
            .get(&char_id)
            .is_some_and(|c| c.clone_grade <= 1);
        let sk = snap.skills.get(&char_id);
        let neg = sk.and_then(|m| m.get(&3356)).map_or(0, |s| s.level);
        let conn = conn_skill_id
            .and_then(|sid| sk.and_then(|m| m.get(&sid)))
            .map_or(0, |s| s.level);
        (alpha, neg, conn)
    });

    let sec_factor = (1.6288 - true_sec).max(0.01);
    let conn_factor = 1.0 + 0.10 * (conn_lvl.min(5) as f64);
    let chosen_base_lp = if is_alpha {
        def.base_lp_alpha
    } else {
        def.base_lp
    };
    let lp_reward = if !def.grants_lp {
        0
    } else {
        ((chosen_base_lp as f64) * sec_factor * conn_factor).round() as i64
    };

    let neg_factor = 1.0 + 0.05 * (neg_lvl.min(5) as f64);
    let isk_reward = ((def.base_isk as f64) * neg_factor).round() as i64;
    let bonus_reward = ((def.time_bonus_isk as f64) * neg_factor).round() as i64;

    CharacterMissionRewards {
        isk_reward,
        lp_reward,
        bonus_reward,
    }
}

pub fn resolve_target_agent_id(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    sys_id: u32,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> (i64, bool) {
    resolve_target_agent_id_with_bound(ctx, session, None, char_id, sys_id, args, _kwargs)
}

pub fn resolve_target_agent_id_with_bound(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&crate::router::BoundObjectEntry>,
    char_id: i64,
    sys_id: u32,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> (i64, bool) {
    if let Some(bound_agent) = bound_entry
        .filter(|b| b.service == "agentMgr" && b.object_id > 0)
        .map(|b| b.object_id)
    {
        return (bound_agent, true);
    }
    if let Some(first_val) = args.first().and_then(|a| a.as_i64()) {
        if ctx.sde.agents.contains_key(&first_val) {
            return (first_val, true);
        }
    }
    if let Some(second_val) = args.get(1).and_then(|a| a.as_i64()) {
        if ctx.sde.agents.contains_key(&second_val) {
            return (second_val, true);
        }
    }
    if let Some(bound_agent) = ctx.bound_objects.read().ok().and_then(|map| {
        map.values()
            .filter(|b| {
                b.session_id == session.session_id && b.service == "agentMgr" && b.object_id > 0
            })
            .max_by_key(|b| b.bound_id)
            .map(|b| b.object_id)
    }) {
        return (bound_agent, true);
    }
    let (default_aid, _) =
        resolve_agent_and_corp_for_system(ctx, char_id, sys_id, session.station_id, false);
    (default_aid, false)
}

pub fn resolve_target_mission_id(
    ctx: &ServerContext,
    char_id: i64,
    agent_id: i64,
    expected_state: Option<MissionState>,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Result<u32, BridgeRpcError> {
    if let Some(second_arg) = args.get(1).and_then(|a| a.as_i64()) {
        if let Ok(mid) = u32::try_from(second_arg) {
            if ctx.sde.missions.contains_key(&mid)
                || ctx.db.with_snapshot(|snap| {
                    snap.missions.contains_character_template(char_id, mid)
                        || snap
                            .missions
                            .latest_history_for_character_template(char_id, mid)
                            .is_some()
                })
            {
                return Ok(mid);
            }
        }
    }
    if let Some(first_arg) = args.first().and_then(|a| a.as_i64()) {
        if first_arg > 19 {
            if let Ok(mid) = u32::try_from(first_arg) {
                if ctx.sde.missions.contains_key(&mid)
                    || ctx.db.with_snapshot(|snap| {
                        snap.missions.contains_character_template(char_id, mid)
                            || snap
                                .missions
                                .latest_history_for_character_template(char_id, mid)
                                .is_some()
                    })
                {
                    return Ok(mid);
                }
            }
        }
    }
    let found = ctx.db.with_snapshot(|snap| {
        snap.missions
            .iter_by_character(char_id)
            .filter(|m| {
                (agent_id <= 0 || m.agent_id == agent_id)
                    && expected_state.map_or(m.state.is_active(), |st| m.state == st)
            })
            .map(|m| m.template_id)
            .min()
            .or_else(|| {
                (agent_id <= 0)
                    .then(|| {
                        snap.missions
                            .iter_by_character(char_id)
                            .filter(|m| expected_state.is_none_or(|st| m.state == st))
                            .map(|m| m.template_id)
                            .min()
                    })
                    .flatten()
            })
            .or_else(|| {
                snap.missions
                    .iter_by_character(char_id)
                    .filter(|m| agent_id <= 0 || m.agent_id == agent_id)
                    .map(|m| m.template_id)
                    .min()
            })
    });
    found.ok_or_else(|| BridgeRpcError {
        code: "MissionNotActive".into(),
        message: format!(
            "No matching active mission found for character {char_id} and agent {agent_id}"
        ),
        client_error: None,
    })
}

pub fn reclaim_courier_cargo(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    mission_id: u32,
) {
    let courier_grant = snap
        .missions
        .get_by_character_template(char_id, mission_id)
        .and_then(|m| m.courier().map(|c| (c.granted_item_id, c.granted_qty)));
    if let Some((cid, granted_qty)) = courier_grant {
        // Only run reclaim for courier missions that were actually accepted (R-MSN-RECLAIM-01, R-MSN-MINT-03)
        if cid.is_some() || granted_qty > 0 {
            let tag = format!("courier_cargo:{char_id}:{mission_id}");
            let mut remaining_to_reclaim = granted_qty.max(1);
            let tagged_items: Vec<(i64, i32)> = snap
                .items
                .iter()
                .filter(|(&iid, it)| {
                    it.owner_id == char_id
                        && (Some(iid) == cid
                            || snap
                                .item_custom_names
                                .get(&iid)
                                .map_or(false, |name| name == &tag))
                })
                .map(|(&iid, it)| (iid, it.quantity))
                .collect();
            for (iid, qty) in tagged_items {
                if qty <= remaining_to_reclaim {
                    snap.remove_system_cleanup_item(iid);
                    remaining_to_reclaim -= qty;
                } else {
                    if let Some(it) = snap.items.get_mut(&iid) {
                        it.quantity -= remaining_to_reclaim;
                        it.version += 1;
                    }
                    snap.item_custom_names.remove(&iid);
                    remaining_to_reclaim = 0;
                }
            }
            let _ = snap.missions
                .update_by_character_template(char_id, mission_id, |m| {
                    if let Some(c) = m.courier_mut() {
                        c.granted_item_id = None;
                        c.granted_qty = 0;
                    }
                });
        }
    }
}

pub async fn abandon_or_decline_mission(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    mission_id: u32,
) -> Result<(String, bool, bool), BridgeRpcError> {
    let (m_sys_id, ball_ids, new_state, penalty_applied, was_accepted) =
        ctx.db.try_with_mut_snapshot(|snap| {
            let Some(existing) = snap.missions.get_by_character_template(char_id, mission_id)
            else {
                return Err(BridgeRpcError {
                    code: "MissionNotActive".into(),
                    message: format!(
                        "Mission {mission_id} is not active or offered for character {char_id}"
                    ),
                    client_error: None,
                });
            };
            if !existing.state.is_active() {
                return Err(BridgeRpcError {
                    code: "MissionNotActive".into(),
                    message: format!(
                        "Mission {mission_id} is in terminal state '{}' and cannot be abandoned",
                        existing.state.as_str()
                    ),
                    client_error: None,
                });
            }
            let was_accepted = existing.state == MissionState::Accepted;
            let m_sys_id = existing.solar_system_id;
            let m_agent_id = existing.agent_id;
            let m_bmid = existing.bookmark_id;
            let ball_ids = existing.spawned_npc_ball_ids().to_vec();
            let next_state = if was_accepted {
                MissionState::Quit
            } else {
                MissionState::Declined
            };
            let new_state = next_state.as_str().to_string();
            let now = ctx.now_unix();
            let mut standing_penalty_applied = false;

            // Reclaim granted courier cargo while active record is still present
            reclaim_courier_cargo(snap, char_id, mission_id);
            let corp_id = ctx
                .sde
                .agents
                .get(&m_agent_id)
                .map(|a| a.corporation_id)
                .unwrap_or_else(|| {
                    let preferred_station = session
                        .station_id
                        .or_else(|| snap.characters.get(&char_id).map(|c| c.home_station_id));
                    resolve_agent_and_corp_for_system_with_preferred_station(
                        ctx,
                        m_sys_id,
                        preferred_station,
                        false,
                    )
                    .1
                });

            let is_rnd = ctx
                .sde
                .agents
                .get(&m_agent_id)
                .is_some_and(|a| is_rnd_agent(a));

            if was_accepted {
                if ctx.sde.missions.contains_key(&mission_id) {
                    let mut actual_delta = None;
                    if let Some(ch) = snap.characters.get_mut(&char_id) {
                        let cur = ch.standings.get(&corp_id).copied().unwrap_or(0.0);
                        let next = (cur - 0.1).max(-10.0);
                        ch.standings.insert(corp_id, next);
                        let ag_cur = ch.standings.get(&m_agent_id).copied().unwrap_or(0.0);
                        ch.standings.insert(m_agent_id, (ag_cur - 0.1).max(-10.0));
                        actual_delta =
                            Some(if cur - 0.1 >= -10.0 { -0.1 } else { -10.0 - cur });
                    }
                    if let Some(delta) = actual_delta {
                        snap.next_entity_id += 1;
                        let ev_id = snap.next_entity_id;
                        snap.standing_transactions
                            .entry(char_id)
                            .or_default()
                            .push(StandingTransactionRecord {
                                event_id: ev_id,
                                from_id: corp_id,
                                to_id: char_id,
                                modification: delta,
                                event_type_id: 83,
                            });
                    }
                    standing_penalty_applied = true;
                }
                if let Some(bmid) = m_bmid {
                    snap.bookmarks.remove(&bmid);
                } else {
                    snap.bookmarks.retain(|_, b| {
                        !(b.owner_id == char_id
                            && b.solar_system_id == m_sys_id
                            && b.memo == "Mission Pocket Destination")
                    });
                }
            } else if !is_rnd {
                // R&D agent mission decline has no standing penalty and does not stop research
                let last_decline = snap
                    .last_mission_decline_times
                    .get(&char_id)
                    .copied()
                    .unwrap_or(0);
                if now.saturating_sub(last_decline) < 14_400 {
                    let mut actual_delta = None;
                    if let Some(ch) = snap.characters.get_mut(&char_id) {
                        let cur = ch.standings.get(&corp_id).copied().unwrap_or(0.0);
                        let next = (cur - 0.05).max(-10.0);
                        ch.standings.insert(corp_id, next);
                        let ag_cur = ch.standings.get(&m_agent_id).copied().unwrap_or(0.0);
                        ch.standings.insert(m_agent_id, (ag_cur - 0.05).max(-10.0));
                        actual_delta =
                            Some(if cur - 0.05 >= -10.0 { -0.05 } else { -10.0 - cur });
                    }
                    if let Some(delta) = actual_delta {
                        snap.next_entity_id += 1;
                        let ev_id = snap.next_entity_id;
                        snap.standing_transactions
                            .entry(char_id)
                            .or_default()
                            .push(StandingTransactionRecord {
                                event_id: ev_id,
                                from_id: corp_id,
                                to_id: char_id,
                                modification: delta,
                                event_type_id: 83,
                            });
                    }
                    standing_penalty_applied = true;
                }
                snap.last_mission_decline_times.insert(char_id, now);
            }
            snap.missions
                .archive_by_character_template(char_id, mission_id, next_state, now)
                .map_err(mission_store_rpc_err)?;
            Ok((
                m_sys_id,
                ball_ids,
                new_state,
                standing_penalty_applied,
                was_accepted,
            ))
        })?;

    if m_sys_id > 0 && !ball_ids.is_empty() {
        let handle = ctx.systems.get_or_spawn(m_sys_id);
        for bid in ball_ids {
            let _ = handle.remove_ball(bid).await;
        }
    }

    Ok((new_state, penalty_applied, was_accepted))
}

#[derive(Debug, Clone)]
pub struct CourierMissionDetails {
    pub cargo_type_id: eve_db::TypeId,
    pub cargo_quantity: i32,
    pub destination_station_id: i64,
    pub origin_station_id: i64,
}

// DERIVED: Courier mission cargo type and quantity formulaically derived from mission title keywords and template ID
pub fn resolve_courier_mission_details(
    ctx: &ServerContext,
    tmpl: &eve_sde::MissionTemplate,
    agent: &eve_sde::AgentInfo,
) -> Result<CourierMissionDetails, BridgeRpcError> {
    let title_lower = tmpl.title.to_lowercase();
    // DERIVED: Cargo type ID and quantity mapped from mission title keywords
    let (cargo_type_id, cargo_quantity) = if title_lower.contains("marine") {
        (3810, 10 + ((tmpl.mission_id % 5) as i32) * 2)
    } else if title_lower.contains("militant") {
        (25373, 10 + ((tmpl.mission_id % 5) as i32) * 2)
    } else if title_lower.contains("arms") || title_lower.contains("weapon") {
        (9844, 10 + ((tmpl.mission_id % 6) as i32) * 2)
    } else if title_lower.contains("drug")
        || title_lower.contains("medicine")
        || title_lower.contains("antibiotic")
    {
        (43, 20 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("party") || title_lower.contains("spirits") {
        (9850, 20 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("tobacco") {
        (9852, 20 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("quafe") {
        (3699, 30 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("wheat") || title_lower.contains("food") {
        (3673, 25 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("tool") || title_lower.contains("breakdown") {
        (21632, 10 + ((tmpl.mission_id % 5) as i32) * 2)
    } else if title_lower.contains("document")
        || title_lower.contains("report")
        || title_lower.contains("data")
        || title_lower.contains("secret")
        || title_lower.contains("tidings")
        || title_lower.contains("lead")
        || title_lower.contains("outbreak")
        || title_lower.contains("interrogation")
        || title_lower.contains("piracy")
        || title_lower.contains("gift")
    {
        (3814, 5 + ((tmpl.mission_id % 5) as i32))
    } else {
        let fallback_pool: [eve_db::TypeId; 8] = [3814, 3810, 25373, 9844, 43, 9850, 3699, 21632];
        let tid = fallback_pool[(tmpl.mission_id as usize) % fallback_pool.len()];
        let qty = 10 + ((tmpl.mission_id % 7) as i32) * 2;
        (tid, qty)
    };

    let agent_station = agent.station_id.or_else(|| {
        agent.solar_system_id.and_then(|sys| {
            ctx.sde.stations.values().filter(|st| st.solar_system_id == sys).map(|st| st.station_id).min()
        })
    }).ok_or_else(|| BridgeRpcError {
        code: "InvalidMissionLocation".into(),
        message: format!("Agent {} has no station or solar system in SDE", agent.agent_id),
        client_error: None,
    })?;

    let agent_sys = agent.solar_system_id.or_else(|| {
        ctx.sde.stations.get(&agent_station).map(|st| st.solar_system_id)
    }).ok_or_else(|| BridgeRpcError {
        code: "AgentMissingSolarSystem".into(),
        message: format!("Agent {} has no resolvable solar system in SDE", agent.agent_id),
        client_error: None,
    })?;
    let agent_constellation = ctx.sde.solar_systems.get(&agent_sys).map(|s| s.constellation_id);

    let mut candidate_stations: Vec<i64> = ctx
        .sde
        .stations
        .values()
        .filter(|st| {
            st.station_id != agent_station
                && ctx
                    .sde
                    .solar_systems
                    .get(&st.solar_system_id)
                    .map(|s| s.constellation_id)
                    == agent_constellation
        })
        .map(|st| st.station_id)
        .collect();
    candidate_stations.sort_unstable();

    // DERIVED: Destination station selected deterministically from reachable candidate stations in constellation or region
    let destination_station_id = if !candidate_stations.is_empty() {
        candidate_stations[(tmpl.mission_id as usize) % candidate_stations.len()]
    } else {
        let agent_region = ctx.sde.solar_systems.get(&agent_sys).map(|s| s.region_id);
        let mut reg_stations: Vec<i64> = ctx
            .sde
            .stations
            .values()
            .filter(|st| {
                st.station_id != agent_station
                    && ctx
                        .sde
                        .solar_systems
                        .get(&st.solar_system_id)
                        .map(|s| s.region_id)
                        == agent_region
            })
            .map(|st| st.station_id)
            .collect();
        reg_stations.sort_unstable();
        if !reg_stations.is_empty() {
            reg_stations[(tmpl.mission_id as usize) % reg_stations.len()]
        } else {
            ctx.sde
                .stations
                .keys()
                .copied()
                .find(|&sid| sid != agent_station)
                .ok_or_else(|| BridgeRpcError {
                    code: "NoDestinationStationAvailable".into(),
                    message: format!("No destination station available for courier mission {}", tmpl.mission_id),
                    client_error: None,
                })?
        }
    };

    Ok(CourierMissionDetails {
        cargo_type_id,
        cargo_quantity,
        destination_station_id,
        origin_station_id: agent_station,
    })
}

#[derive(Debug, Clone)]
pub struct TradeMissionDetails {
    pub trade_type_id: eve_db::TypeId,
    pub trade_quantity: i32,
}

// DERIVED: Trade mission required commodity type and quantity derived from mission title keywords and template ID
pub fn resolve_trade_mission_details(
    _ctx: &ServerContext,
    tmpl: &eve_sde::MissionTemplate,
) -> TradeMissionDetails {
    let title_lower = tmpl.title.to_lowercase();
    // DERIVED: Commodity type ID and quota mapped from title keywords and template ID
    let (trade_type_id, trade_quantity) = if title_lower.contains("trit") {
        (34, 1_000 + ((tmpl.mission_id % 10) as i32) * 100)
    } else if title_lower.contains("entertainment") || title_lower.contains("holoreel") {
        (3647, 10 + ((tmpl.mission_id % 5) as i32) * 5)
    } else if title_lower.contains("microorganism") || title_lower.contains("antibiotic") {
        (43, 25 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("oil") || title_lower.contains("synthetic") {
        (3691, 15 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("wheat") || title_lower.contains("grain") {
        (3673, 25 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("oxygen") || title_lower.contains("gas") {
        (3683, 20 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("livestock") || title_lower.contains("cattle") || title_lower.contains("animal") {
        (3725, 10 + ((tmpl.mission_id % 5) as i32) * 5)
    } else if title_lower.contains("food") || title_lower.contains("goods") {
        (3673, 20 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("drill") || title_lower.contains("tool") {
        (21632, 10 + ((tmpl.mission_id % 5) as i32) * 2)
    } else if title_lower.contains("laser") || title_lower.contains("electronic") {
        (3687, 15 + ((tmpl.mission_id % 5) as i32) * 5)
    } else if title_lower.contains("wine") || title_lower.contains("spirits") {
        (42, 20 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("pyroxeres") {
        (1224, 200 + ((tmpl.mission_id % 5) as i32) * 50)
    } else if title_lower.contains("scordite") {
        (1228, 300 + ((tmpl.mission_id % 5) as i32) * 50)
    } else {
        let pool: [(eve_db::TypeId, i32); 8] = [
            (43, 25),    // Antibiotics
            (3691, 20),  // Synthetic Oil
            (3673, 30),  // Wheat
            (3683, 25),  // Oxygen
            (3725, 10),  // Livestock
            (3647, 15),  // Holoreels
            (34, 1000),  // Tritanium
            (21632, 10), // Tools
        ];
        let (tid, base_qty) = pool[(tmpl.mission_id as usize) % pool.len()];
        let qty = if tid == 34 {
            base_qty + ((tmpl.mission_id % 10) as i32) * 100
        } else {
            base_qty + ((tmpl.mission_id % 5) as i32) * 5
        };
        (tid, qty)
    };

    TradeMissionDetails {
        trade_type_id,
        trade_quantity,
    }
}

/// Computes a deterministic SplitMix64 seed from character, agent, and archived offer count.
pub fn mission_offer_selection_seed(char_id: i64, agent_id: i64, history_count: u64) -> u64 {
    let mut s = (char_id as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93);
    s ^= (agent_id as u64).wrapping_mul(0xA076_1D64_78BD_642F);
    s.rotate_left(17) ^ history_count.wrapping_mul(0xE703_7ED1_A0B4_28DB)
}

/// Deterministically selects a repeatable mission template using SplitMix64 recency weighting.
///
/// Sorts candidates by `mission_id`, excludes `recent_templates.first()` when `candidates.len() >= 2`
/// to avoid immediate back-to-back repeats, and assigns weight `4` to non-recent templates vs `1`
/// to recent templates.
pub fn select_weighted_mission_template<'a>(
    candidates: &[&'a eve_sde::MissionTemplate],
    recent_templates: &[u32],
    seed: u64,
) -> Option<&'a eve_sde::MissionTemplate> {
    if candidates.is_empty() {
        return None;
    }
    let mut sorted: Vec<&'a eve_sde::MissionTemplate> = candidates.to_vec();
    sorted.sort_by_key(|m| m.mission_id);

    let pool: Vec<&'a eve_sde::MissionTemplate> = if sorted.len() >= 2 {
        if let Some(&immediate_prev) = recent_templates.first() {
            let filtered: Vec<&'a eve_sde::MissionTemplate> = sorted
                .iter()
                .copied()
                .filter(|m| m.mission_id != immediate_prev)
                .collect();
            if filtered.is_empty() {
                sorted
            } else {
                filtered
            }
        } else {
            sorted
        }
    } else {
        sorted
    };

    let total_weight: u64 = pool
        .iter()
        .map(|m| if recent_templates.contains(&m.mission_id) { 1 } else { 4 })
        .sum();
    if total_weight == 0 {
        return pool.first().copied();
    }

    let mut x = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    let mixed = x ^ (x >> 31);
    let mut roll = mixed % total_weight;
    for m in &pool {
        let w = if recent_templates.contains(&m.mission_id) {
            1
        } else {
            4
        };
        if roll < w {
            return Some(*m);
        }
        roll -= w;
    }
    pool.first().copied()
}

/// Resolved ship hull requirement for a Paragon Heraldry turn-in mission (`PVE-018`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeraldryMissionDetails {
    pub ship_type_id: eve_db::TypeId,
    pub quantity: i32,
}

/// Deterministically resolves an SDE-verified T1 packaged ship hull for a Paragon Heraldry mission.
pub fn resolve_heraldry_mission_details(
    ctx: &ServerContext,
    tmpl: &eve_sde::MissionTemplate,
) -> HeraldryMissionDetails {
    let tier: &[eve_db::TypeId] = if tmpl.lp_omega <= 2_500 {
        &[583, 585, 587, 589, 594, 597, 603, 608]
    } else if tmpl.lp_omega <= 3_000 {
        &[16236, 16238, 16240, 16242, 32880]
    } else if tmpl.lp_omega <= 3_600 {
        &[648, 650, 652, 1944]
    } else {
        &[621, 624, 627, 629]
    };
    let picked = tier[(tmpl.mission_id as usize) % tier.len()];
    let ship_type_id = if ctx.sde.types.contains_key(&picked) {
        picked
    } else {
        tier.iter()
            .copied()
            .find(|tid| ctx.sde.types.contains_key(tid))
            .unwrap_or(587)
    };
    HeraldryMissionDetails {
        ship_type_id,
        quantity: 1,
    }
}

#[derive(Debug, Clone)]
pub struct MiningMissionDetails {
    pub ore_type_id: eve_db::TypeId,
    pub required_units: i32,
    pub objective_text: String,
}

// DERIVED: Mining mission target ore type and quantity derived from mission title keywords and template ID
pub fn resolve_mining_mission_details(
    ctx: &ServerContext,
    tmpl: &eve_sde::MissionTemplate,
) -> MiningMissionDetails {
    let title_lower = tmpl.title.to_lowercase();
    // DERIVED: Ore type ID and required units mapped from title keywords
    let (ore_type_id, required_units) = if title_lower.contains("banidine") {
        (28617, 500)
    } else if title_lower.contains("augumene") {
        (28618, 500)
    } else if title_lower.contains("mercium") {
        (28619, 500)
    } else if title_lower.contains("lyavite") {
        (28620, 250)
    } else if title_lower.contains("pithix") {
        (28621, 250)
    } else if title_lower.contains("arisite") {
        (28622, 100)
    } else if title_lower.contains("geodite") {
        (28624, 50)
    } else if title_lower.contains("gneiss") {
        (1229, 100)
    } else if title_lower.contains("omber") {
        (1227, 500)
    } else if title_lower.contains("kernite") {
        (20, 250)
    } else if title_lower.contains("pyroxeres") {
        (1224, 500)
    } else if title_lower.contains("scordite") {
        (1228, 500)
    } else if title_lower.contains("plagioclase") {
        (18, 500)
    } else if title_lower.contains("veldspar") {
        (1230, 500)
    } else {
        let pool: [(eve_db::TypeId, i32); 4] = [
            (1230, 500),  // Veldspar
            (1228, 500),  // Scordite
            (1224, 500),  // Pyroxeres
            (20, 250),    // Kernite
        ];
        pool[(tmpl.mission_id as usize) % pool.len()]
    };

    let ore_name = ctx
        .sde
        .types
        .get(&ore_type_id)
        .map(|t| t.name.as_str())
        .unwrap_or("ore");
    let objective_text = format!("Mine {required_units} units of {ore_name} and deliver to the agent station hangar.");

    MiningMissionDetails {
        ore_type_id,
        required_units,
        objective_text,
    }
}

#[derive(Debug, Clone)]
pub struct SecurityMissionEncounter {
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub faction_id: u32,
    pub spawn_group_id: u32,
    pub npc_type_ids: Vec<eve_db::TypeId>,
    pub wave_npc_count: u32,
    pub objective_text: String,
    pub briefing_text: String,
}

// DERIVED: Security mission dungeon encounter, target faction, and wave parameters derived from mission title, agent level, and SDE dungeons
pub fn resolve_security_mission_encounter(
    ctx: &ServerContext,
    tmpl: &eve_sde::MissionTemplate,
    agent: &eve_sde::AgentInfo,
    sys_id: u32,
) -> SecurityMissionEncounter {
    let agent_level = agent.level.clamp(1, 5) as u32;
    let agent_sys_id = agent.solar_system_id.unwrap_or(sys_id);
    let region_id = ctx.sde.solar_systems.get(&agent_sys_id).map(|s| s.region_id);
    let region_name = region_id
        .and_then(|rid| ctx.sde.regions.get(&rid))
        .map(|r| r.name.to_lowercase());
    let sys_name = ctx
        .sde
        .solar_systems
        .get(&agent_sys_id)
        .map(|s| s.name.to_lowercase());
    let region_faction = region_id
        .and_then(|rid| ctx.sde.regions.get(&rid))
        .and_then(|r| r.faction_id);
    let agent_faction = ctx
        .sde
        .npc_corporations
        .get(&agent.corporation_id)
        .and_then(|c| c.faction_id)
        .unwrap_or(0);
    let title_clean = tmpl
        .title
        .to_lowercase()
        .replace(['(', ')', '-', ':', '\''], " ");
    let title_words: Vec<&str> = title_clean
        .split_whitespace()
        .filter(|w| w.len() > 3 && !matches!(*w, "the" | "part" | "level" | "mission"))
        .collect();

    // DERIVED: Dungeon matched from title keywords, system/region, agent faction, archetype 20, or SDE pool
    let matched_dungeon = ctx
        .sde
        .dungeons
        .values()
        .filter(|d| {
            let d_name_low = d.name.to_lowercase();
            tmpl.title.to_lowercase().contains(&d_name_low)
                || d_name_low.contains(&tmpl.title.to_lowercase())
                || title_words.iter().filter(|&&w| d_name_low.contains(w)).count() >= 2
        })
        .min_by_key(|d| {
            let diff_dist = d.difficulty.unwrap_or(agent_level).abs_diff(agent_level);
            (diff_dist, d.dungeon_id)
        })
        .or_else(|| {
            // DERIVED: Filter candidate dungeons by matching system or region
            ctx.sde
                .dungeons
                .values()
                .filter(|d| {
                    let d_name_low = d.name.to_lowercase();
                    let sys_match = sys_name.as_ref().is_some_and(|sn| d_name_low.contains(sn));
                    let reg_match = region_name.as_ref().is_some_and(|rn| d_name_low.contains(rn));
                    let fac_match = region_faction.is_some_and(|rf| d.faction_id == Some(rf));
                    (sys_match || reg_match || fac_match)
                        && d.difficulty.unwrap_or(agent_level) == agent_level
                })
                .min_by_key(|d| (d.dungeon_id as usize + tmpl.mission_id as usize) % 1000)
        })
        .or_else(|| {
            if agent_faction > 0 {
                ctx.sde
                    .dungeons
                    .values()
                    .filter(|d| d.faction_id == Some(agent_faction) && d.difficulty == Some(agent_level))
                    .min_by_key(|d| (d.dungeon_id as usize + tmpl.mission_id as usize) % 1000)
            } else {
                None
            }
        })
        .or_else(|| {
            ctx.sde
                .dungeons
                .values()
                .filter(|d| d.archetype_id == Some(20) && d.difficulty == Some(agent_level))
                .min_by_key(|d| (d.dungeon_id as usize + tmpl.mission_id as usize) % 1000)
        })
        .or_else(|| {
            ctx.sde
                .dungeons
                .values()
                .filter(|d| d.archetype_id == Some(20))
                .min_by_key(|d| d.dungeon_id)
        })
        .or_else(|| {
            ctx.sde.dungeons.values().min_by_key(|d| d.dungeon_id)
        });

    let (dungeon_id, dungeon_name, dungeon_faction, dungeon_difficulty) = match matched_dungeon {
        Some(d) => (
            d.dungeon_id,
            d.name.clone(),
            d.faction_id.unwrap_or(agent_faction),
            d.difficulty.unwrap_or(agent_level),
        ),
        None => (
            0,
            "Encounter Pocket".to_string(),
            agent_faction,
            agent_level,
        ),
    };

    let dungeon_title_lc = dungeon_name.to_lowercase();
    let explicit_sleeper_or_drifter = title_clean.contains("sleeper")
        || title_clean.contains("drifter")
        || dungeon_title_lc.contains("sleeper")
        || dungeon_title_lc.contains("drifter");
    // SDE faction `500021` ("Unknown") is a sentinel for unspecified/generic mission enemies;
    // avoid mapping it to Roaming Sleepers / Autothysian Lancers unless explicitly Sleeper/Drifter.
    let effective_dungeon_faction = if dungeon_faction == 500_021 && !explicit_sleeper_or_drifter {
        0
    } else {
        dungeon_faction
    };

    // DERIVED: Identify hostile faction from dungeon faction, mission title keywords, or agent faction
    let target_faction_id = if effective_dungeon_faction > 0 {
        effective_dungeon_faction
    } else if title_clean.contains("guristas") {
        500010
    } else if title_clean.contains("angel") {
        500011
    } else if title_clean.contains("blood") {
        500012
    } else if title_clean.contains("sansha") {
        500019
    } else if title_clean.contains("serpentis") {
        500020
    } else if agent_faction > 0 {
        agent_faction
    } else {
        ctx.sde.npc_spawn_groups.values().map(|sg| sg.faction_id).find(|&f| f > 0 && f != 500_021).unwrap_or(0)
    };

    let target_faction_name = match target_faction_id {
        500010 => "Guristas",
        500011 => "Angel Cartel",
        500012 => "Blood Raiders",
        500019 => "Sansha's Nation",
        500020 => "Serpentis",
        _ => ctx.sde.factions.get(&target_faction_id).map(|f| f.name.as_str()).unwrap_or("Hostile"),
    };

    // DERIVED: Ship class derived from encounter difficulty level (1 = Frigate, 2 = Cruiser, 3+ = Battleship)
    let target_ship_class = match dungeon_difficulty {
        1 => 1,
        2 => 2,
        3 => 2,
        _ => 3,
    };

    let is_combat_mission_spawn_group = |sg: &eve_sde::NpcSpawnGroupInfo| {
        if sg.npc_type_ids.is_empty() {
            return false;
        }
        // Exclude non-combatant props/structures and invulnerable CONCORD/Police/Sentry groups by canonical SDE group_id
        if crate::services::combat_crimewatch::combat_legacy_ess::is_non_combat_npc_prop_group(sg.group_id)
            || crate::services::combat_crimewatch::combat_legacy_ess::is_invulnerable_npc_authority_group(sg.group_id)
        {
            return false;
        }
        // Exclude roaming/wormhole Sleeper & Drifter groups unless explicitly Sleeper/Drifter
        if !explicit_sleeper_or_drifter
            && target_faction_id != 500_024
            && crate::services::combat_crimewatch::combat_legacy_ess::is_sleeper_or_drifter_npc_group(sg.group_id)
        {
            return false;
        }
        true
    };

    let matching_group = ctx
        .sde
        .npc_spawn_groups
        .values()
        .filter(|sg| {
            let name_low = sg.group_name.to_lowercase();
            is_combat_mission_spawn_group(sg)
                && (name_low.contains(&target_faction_name.to_lowercase()) || sg.faction_id == target_faction_id)
                && sg.ship_class == target_ship_class
        })
        .min_by_key(|sg| (sg.group_id, sg.spawn_group_id))
        .or_else(|| {
            ctx.sde
                .npc_spawn_groups
                .values()
                .filter(|sg| is_combat_mission_spawn_group(sg) && (sg.faction_id == target_faction_id || sg.group_name.contains("Deadspace")))
                .min_by_key(|sg| (sg.ship_class.abs_diff(target_ship_class), sg.group_id, sg.spawn_group_id))
        })
        .or_else(|| {
            ctx.sde
                .npc_spawn_groups
                .values()
                .filter(|sg| is_combat_mission_spawn_group(sg))
                .min_by_key(|sg| (sg.ship_class.abs_diff(target_ship_class), sg.group_id, sg.spawn_group_id))
        });

    let (spawn_group_id, npc_type_ids) = match matching_group {
        Some(sg) => (sg.spawn_group_id, sg.npc_type_ids.clone()),
        None => (0, vec![resolve_mission_npc_type(ctx)]),
    };

    // DERIVED: Wave NPC count scaled by agent level and dungeon difficulty (level 1 / starter = 1, higher levels = 3-6)
    let wave_npc_count = if agent.level <= 1 || dungeon_difficulty <= 1 {
        1
    } else {
        match dungeon_difficulty {
            2 => 3,
            3 => 4,
            4 => 5,
            _ => 6,
        }
    };

    let sys_name = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| format!("System {sys_id}"));

    let objective_text = format!(
        "Infiltrate {} in {} and eliminate all hostile {} vessels ({} enemies).",
        dungeon_name, sys_name, target_faction_name, wave_npc_count
    );
    let briefing_text = format!(
        "Agent {} operation: {}. Hostile {} forces detected at {}. Neutralize the encounter and report back.",
        agent.agent_id, tmpl.title, target_faction_name, dungeon_name
    );

    SecurityMissionEncounter {
        dungeon_id,
        dungeon_name,
        faction_id: target_faction_id,
        spawn_group_id,
        npc_type_ids,
        wave_npc_count,
        objective_text,
        briefing_text,
    }
}

/// Handles destruction of an NPC in space, decrementing npcs_remaining for any active mission tracking that NPC ball.
pub fn on_mission_npc_destroyed_core(
    db: &eve_db::WorldRepository,
    sys_id: u32,
    npc_id: i64,
    _killer_char_id: Option<i64>,
) {
    let should_update = db.with_snapshot(|snap| {
        snap.missions
            .get_by_npc_ball(npc_id)
            .is_some_and(|m| m.state == MissionState::Accepted && m.solar_system_id == sys_id)
    });
    if !should_update {
        return;
    }
    db.with_mut_snapshot(|snap| {
        snap.missions.on_npc_destroyed(sys_id, npc_id);
    });
}

#[cfg(test)]
#[path = "agents_missions_helpers_tests.rs"]
mod agents_missions_helpers_tests;


